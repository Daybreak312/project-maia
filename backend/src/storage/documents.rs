use anyhow::{Context, Result};
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use tokio::fs;
use tokio::sync::{Mutex, MutexGuard};
use uuid::Uuid;

use crate::models::{Document, RelationType};

/// 이웃 탐색 depth 상한. 개인 지식 그래프에서 5-hop을 넘는 탐색은 의미가 옅고,
/// fan-out에 따른 파일 I/O 폭발을 막는 안전장치다.
pub const MAX_NEIGHBOR_DEPTH: usize = 5;

/// 이웃 탐색 결과 개수 상한. 밀집 그래프에서 반환량이 폭발하지 않도록 제한한다.
pub const MAX_NEIGHBOR_RESULTS: usize = 200;

/// 그래프 이웃 탐색 결과의 한 노드.
///
/// 시작 문서로부터 BFS로 도달한 문서와, 그 도달 경로 정보(직전 문서·관계·거리)를
/// 함께 담는다. 호출 측(API)이 "어떤 관계로 몇 hop 떨어져 있는지"를 표시할 수 있다.
#[derive(Debug, Clone)]
pub struct NeighborNode {
    /// 도달한 이웃 문서
    pub document: Document,
    /// 시작 문서로부터의 거리 (1 = 직접 이웃, 2 = 2-hop ...)
    pub depth: usize,
    /// 직전(부모) 문서에서 이 문서로 향한 관계 타입
    pub relation: RelationType,
    /// 직전(부모) 문서 ID — 경로 재구성용
    pub via: Uuid,
    /// 부모→이 문서 엣지의 가중치
    pub weight: f32,
}

/// 원본 문서를 파일 시스템에 저장 (워크스페이스별 경로 분리)
///
/// 저장 경로: `{data_dir}/workspaces/{workspace_id}/documents/{doc_id}.json`
pub struct DocumentStore {
    data_dir: PathBuf,
    /// 문서 쓰기 트랜잭션(load→수정→save) 직렬화 락.
    ///
    /// **lost-update 방지**: raw JSON이 그래프 엣지의 SSoT인데 `save`는 락 없는 전체
    /// 덮어쓰기다. 여러 라이터(엣지 감쇠 재계산·엣지 추가/제거·재파싱 업데이트)가 같은
    /// 문서를 동시에 read-modify-write하면 늦게 저장하는 쪽이 앞선 수정을 조용히 덮어
    /// **엣지가 비가역 소실**된다(reindex도 오염된 raw를 읽어 복원 불가). 이 락으로 모든
    /// 쓰기 트랜잭션을 직렬화해 그 경합을 제거한다. review/freshness/history 저장소와
    /// 동일한 파일 쓰기 직렬화 패턴이다.
    write_lock: Mutex<()>,
}

impl DocumentStore {
    pub async fn new(data_dir: impl Into<PathBuf>) -> Result<Self> {
        let data_dir = data_dir.into();
        Ok(Self {
            data_dir,
            write_lock: Mutex::new(()),
        })
    }

    /// 워크스페이스의 문서 디렉토리 경로
    fn workspace_docs_path(&self, workspace_id: &str) -> PathBuf {
        self.data_dir
            .join("workspaces")
            .join(workspace_id)
            .join("documents")
    }

    pub async fn save(&self, doc: &Document, workspace_id: &str) -> Result<PathBuf> {
        let base = self.workspace_docs_path(workspace_id);
        fs::create_dir_all(&base)
            .await
            .context("Failed to create document storage directory")?;

        let file_path = base.join(format!("{}.json", doc.id));
        let content = serde_json::to_string_pretty(doc)?;

        fs::write(&file_path, content)
            .await
            .context("Failed to write document file")?;

        Ok(file_path)
    }

    pub async fn load(&self, id: Uuid, workspace_id: &str) -> Result<Document> {
        let file_path = self
            .workspace_docs_path(workspace_id)
            .join(format!("{}.json", id));
        let content = fs::read_to_string(&file_path)
            .await
            .context("Failed to read document file")?;

        let doc: Document = serde_json::from_str(&content)?;
        Ok(doc)
    }

    pub async fn exists(&self, id: Uuid, workspace_id: &str) -> bool {
        self.workspace_docs_path(workspace_id)
            .join(format!("{}.json", id))
            .exists()
    }

    pub async fn delete(&self, id: Uuid, workspace_id: &str) -> Result<()> {
        let file_path = self
            .workspace_docs_path(workspace_id)
            .join(format!("{}.json", id));
        fs::remove_file(&file_path)
            .await
            .context("Failed to delete document file")?;
        Ok(())
    }

    /// 문서를 **원자적으로** load→수정→save 한다.
    ///
    /// 같은 저장소의 다른 쓰기 트랜잭션과 [`write_lock`](Self::write_lock)으로 직렬화되어
    /// 동시 수정의 lost-update를 제거한다(예: 엣지 감쇠 재저장이 방금 추가된 엣지를 덮어씀).
    /// `mutate`는 **락 아래에서 디스크로부터 갓 로드된 최신 문서**를 받으므로, 스냅샷이
    /// 아니라 항상 현재 상태를 기준으로 수정한다(stale 스냅샷 저장 회귀 차단).
    ///
    /// - `mutate`가 `true`를 반환할 때만 저장한다(불필요한 쓰기 억제).
    /// - 문서가 없으면 `Ok(None)`(경합 중 삭제/멱등 재제출 대응 — 에러 아님).
    /// - 저장(또는 미변경)된 최신 문서를 반환한다.
    ///
    /// `mutate`는 동기 클로저다 — 로드와 저장 **사이에 비동기 작업**(예: 버전 보관)이 필요한
    /// 복합 트랜잭션은 [`write_guard`](Self::write_guard)로 직접 임계 구역을 구성하라.
    pub async fn update<F>(
        &self,
        id: Uuid,
        workspace_id: &str,
        mutate: F,
    ) -> Result<Option<Document>>
    where
        F: FnOnce(&mut Document) -> bool,
    {
        let _guard = self.write_lock.lock().await;
        if !self.exists(id, workspace_id).await {
            return Ok(None); // 경합 중 삭제됨 — 멱등 처리(이중 판단/재제출 안전)
        }
        let mut doc = self.load(id, workspace_id).await?;
        if mutate(&mut doc) {
            self.save(&doc, workspace_id).await?;
        }
        Ok(Some(doc))
    }

    /// 쓰기 트랜잭션 임계 구역 가드.
    ///
    /// `load→(비동기 작업)→save`를 이 가드 아래에서 수행하면 다른 쓰기와 직렬화된다.
    /// 버전 보관처럼 로드와 저장 사이에 비동기 작업이 끼는 복합 트랜잭션용 탈출구다 —
    /// 순수 동기 수정은 [`update`](Self::update)를 쓰라.
    ///
    /// **주의(재진입 금지):** 이 가드를 쥔 채 같은 저장소의 `update`/`write_guard`를 다시
    /// 호출하면 자기 자신을 기다려 데드락한다. 가드 아래에서는 `load`/`save`만 직접 부른다.
    pub async fn write_guard(&self) -> MutexGuard<'_, ()> {
        self.write_lock.lock().await
    }

    pub async fn list_recent(&self, limit: usize, workspace_id: &str) -> Result<Vec<Document>> {
        let base = self.workspace_docs_path(workspace_id);

        // 디렉토리가 없으면 빈 목록 반환
        if !base.exists() {
            return Ok(Vec::new());
        }

        let mut entries = fs::read_dir(&base)
            .await
            .context("Failed to read document directory")?;

        let mut docs = Vec::new();

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "json") {
                if let Ok(content) = fs::read_to_string(&path).await {
                    if let Ok(doc) = serde_json::from_str::<Document>(&content) {
                        docs.push(doc);
                    }
                }
            }
        }

        // 최신순 정렬
        docs.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        docs.truncate(limit);

        Ok(docs)
    }

    /// 워크스페이스에서 `(source_type, source_id)`가 일치하는 문서를 찾는다.
    ///
    /// 커넥터 재유입 시 "이미 유입된 소스 항목인가"를 판별하는 조회다 — 일치하면
    /// 신규 생성이 아니라 업데이트 경로로 보내 문서 난립을 막는다. raw JSON(SSoT)만
    /// 스캔하므로 Qdrant 없이 동작하고, 손상되었거나 파싱 불가한 파일은 조용히 건너뛴다
    /// (하나의 깨진 파일이 조회 전체를 실패시키지 않는다).
    ///
    /// 디렉토리 전체를 스캔하므로 대량 적재에서 문서 수에 비례하는 비용이 있으나, 유입은
    /// LLM 파싱 지연이 지배적이라 이 스캔은 상대적으로 무시 가능하다.
    pub async fn find_by_source(
        &self,
        source_type: &str,
        source_id: &str,
        workspace_id: &str,
    ) -> Result<Option<Document>> {
        let base = self.workspace_docs_path(workspace_id);
        if !base.exists() {
            return Ok(None);
        }

        let mut entries = fs::read_dir(&base)
            .await
            .context("Failed to read document directory")?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let Ok(content) = fs::read_to_string(&path).await else {
                continue;
            };
            let Ok(doc) = serde_json::from_str::<Document>(&content) else {
                continue;
            };
            if let Some(source) = &doc.source {
                if source.source_type == source_type && source.source_id == source_id {
                    return Ok(Some(doc));
                }
            }
        }

        Ok(None)
    }

    /// 시작 문서로부터 엣지를 따라 BFS로 이웃 문서들을 탐색한다.
    ///
    /// - `depth`는 `[1, MAX_NEIGHBOR_DEPTH]`로 클램프된다(상한 필수).
    /// - `visited` 집합으로 순환 그래프에서도 무한 루프 없이 각 문서를 한 번만 방문한다.
    /// - BFS(FIFO)라 각 이웃의 `depth`는 시작 문서로부터의 **최단** 거리다.
    /// - dangling 엣지(대상 문서가 없음)는 조용히 건너뛴다(그래프 정합성 방어).
    /// - 결과는 `MAX_NEIGHBOR_RESULTS`로 상한된다.
    ///
    /// raw JSON(SSoT)만 사용하므로 Qdrant 없이 동작하고 tempdir로 단위 테스트된다.
    pub async fn neighbors(
        &self,
        start_id: Uuid,
        depth: usize,
        workspace_id: &str,
    ) -> Result<Vec<NeighborNode>> {
        let max_depth = depth.clamp(1, MAX_NEIGHBOR_DEPTH);

        let mut visited: HashSet<Uuid> = HashSet::new();
        visited.insert(start_id); // 시작 문서는 자기 자신이므로 결과에 포함하지 않는다
        let mut result: Vec<NeighborNode> = Vec::new();
        let mut queue: VecDeque<(Uuid, usize)> = VecDeque::new();
        queue.push_back((start_id, 0));

        while let Some((current_id, current_depth)) = queue.pop_front() {
            // 상한 도달 노드는 확장하지 않는다.
            if current_depth >= max_depth {
                continue;
            }

            // 현재 문서 로드 실패(없음) 시 이 가지는 확장하지 않는다.
            let doc = match self.load(current_id, workspace_id).await {
                Ok(d) => d,
                Err(_) => continue,
            };

            for edge in &doc.edges {
                if visited.contains(&edge.target) {
                    continue; // 이미 방문 — 순환/중복 방지
                }
                visited.insert(edge.target);

                // 대상 문서를 로드해 결과에 포함 (dangling 엣지는 건너뜀)
                if let Ok(target_doc) = self.load(edge.target, workspace_id).await {
                    let next_depth = current_depth + 1;
                    result.push(NeighborNode {
                        document: target_doc,
                        depth: next_depth,
                        relation: edge.relation,
                        via: current_id,
                        weight: edge.weight,
                    });
                    queue.push_back((edge.target, next_depth));

                    if result.len() >= MAX_NEIGHBOR_RESULTS {
                        return Ok(result);
                    }
                }
            }
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Document;
    use tempfile::TempDir;

    fn make_doc(content: &str) -> Document {
        Document::new(
            content.to_string(),
            format!("Summary of {}", content),
            vec![],
            vec![],
        )
    }

    async fn setup() -> (TempDir, DocumentStore) {
        let tmp = TempDir::new().unwrap();
        let store = DocumentStore::new(tmp.path()).await.unwrap();
        (tmp, store)
    }

    #[tokio::test]
    async fn test_save_and_load() {
        let (_tmp, store) = setup().await;
        let doc = make_doc("hello world");
        let id = doc.id;

        store.save(&doc, "default").await.unwrap();
        let loaded = store.load(id, "default").await.unwrap();

        assert_eq!(loaded.id, id);
        assert_eq!(loaded.raw_content, "hello world");
    }

    #[tokio::test]
    async fn test_save_creates_workspace_dir() {
        let (tmp, store) = setup().await;
        let doc = make_doc("test");

        store.save(&doc, "my-ws").await.unwrap();

        let ws_dir = tmp.path().join("workspaces/my-ws/documents");
        assert!(ws_dir.exists());
    }

    #[tokio::test]
    async fn test_load_nonexistent_fails() {
        let (_tmp, store) = setup().await;
        let result = store.load(Uuid::new_v4(), "default").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_workspace_isolation() {
        let (_tmp, store) = setup().await;
        let doc = make_doc("isolated");
        let id = doc.id;

        store.save(&doc, "ws-a").await.unwrap();

        assert!(store.exists(id, "ws-a").await);
        assert!(!store.exists(id, "ws-b").await);
        assert!(store.load(id, "ws-b").await.is_err());
    }

    #[tokio::test]
    async fn test_delete() {
        let (_tmp, store) = setup().await;
        let doc = make_doc("will delete");
        let id = doc.id;

        store.save(&doc, "default").await.unwrap();
        assert!(store.exists(id, "default").await);

        store.delete(id, "default").await.unwrap();
        assert!(!store.exists(id, "default").await);
    }

    #[tokio::test]
    async fn test_delete_nonexistent_fails() {
        let (_tmp, store) = setup().await;
        let result = store.delete(Uuid::new_v4(), "default").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_list_recent_empty_workspace() {
        let (_tmp, store) = setup().await;
        let docs = store.list_recent(10, "empty-ws").await.unwrap();
        assert!(docs.is_empty());
    }

    #[tokio::test]
    async fn test_list_recent_returns_docs() {
        let (_tmp, store) = setup().await;

        store.save(&make_doc("first"), "default").await.unwrap();
        store.save(&make_doc("second"), "default").await.unwrap();

        let docs = store.list_recent(10, "default").await.unwrap();
        assert_eq!(docs.len(), 2);
    }

    #[tokio::test]
    async fn test_list_recent_respects_limit() {
        let (_tmp, store) = setup().await;

        for i in 0..5 {
            store
                .save(&make_doc(&format!("doc {}", i)), "default")
                .await
                .unwrap();
        }

        let docs = store.list_recent(3, "default").await.unwrap();
        assert_eq!(docs.len(), 3);
    }

    #[tokio::test]
    async fn test_same_id_coexists_across_workspaces() {
        // 동일 문서 ID가 서로 다른 워크스페이스에서 충돌 없이 존재해야 한다.
        let (_tmp, store) = setup().await;
        let shared_id = Uuid::new_v4();

        let mut doc_a = make_doc("personal content");
        doc_a.id = shared_id;
        let mut doc_b = make_doc("work content");
        doc_b.id = shared_id;

        store.save(&doc_a, "personal").await.unwrap();
        store.save(&doc_b, "work").await.unwrap();

        // 각 워크스페이스에서 같은 ID를 로드해도 서로 다른 내용이 나와야 한다
        let loaded_a = store.load(shared_id, "personal").await.unwrap();
        let loaded_b = store.load(shared_id, "work").await.unwrap();

        assert_eq!(loaded_a.raw_content, "personal content");
        assert_eq!(loaded_b.raw_content, "work content");
        assert_eq!(loaded_a.id, loaded_b.id, "ID는 같지만 격리되어 있어야 한다");
    }

    #[tokio::test]
    async fn test_save_and_load_preserves_edges() {
        // raw JSON이 그래프 엣지의 SSoT — 저장/로드 왕복에서 보존되어야 한다.
        // (이 보존이 reindex 엣지 생존의 raw 측 불변식이다.)
        use crate::models::{Edge, RelationType};
        let (_tmp, store) = setup().await;
        let mut doc = make_doc("with edges");
        let target = Uuid::new_v4();
        doc.add_edge(Edge::new(target, RelationType::Updates, 0.7));
        let id = doc.id;

        store.save(&doc, "default").await.unwrap();
        let loaded = store.load(id, "default").await.unwrap();

        assert_eq!(loaded.edges.len(), 1);
        assert_eq!(loaded.edges[0].target, target);
        assert_eq!(loaded.edges[0].relation, RelationType::Updates);
        assert!((loaded.edges[0].weight - 0.7).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn test_list_recent_workspace_isolation() {
        let (_tmp, store) = setup().await;

        store.save(&make_doc("a"), "ws-a").await.unwrap();
        store.save(&make_doc("b"), "ws-b").await.unwrap();

        let a_docs = store.list_recent(10, "ws-a").await.unwrap();
        let b_docs = store.list_recent(10, "ws-b").await.unwrap();

        assert_eq!(a_docs.len(), 1);
        assert_eq!(b_docs.len(), 1);
        assert_eq!(a_docs[0].raw_content, "a");
        assert_eq!(b_docs[0].raw_content, "b");
    }

    // ──── 쓰기 직렬화 (lost-update 방지) ────

    #[tokio::test]
    async fn test_update_applies_and_saves() {
        use crate::models::{Edge, RelationType};
        let (_tmp, store) = setup().await;
        let doc = make_doc("x");
        let id = doc.id;
        store.save(&doc, "default").await.unwrap();

        let target = Uuid::new_v4();
        let updated = store
            .update(id, "default", |d| {
                d.add_edge(Edge::new(target, RelationType::RelatedTo, 0.5));
                true
            })
            .await
            .unwrap();

        assert!(updated.is_some(), "존재하는 문서 update는 최신 문서를 반환");
        let loaded = store.load(id, "default").await.unwrap();
        assert_eq!(loaded.edges.len(), 1);
        assert_eq!(loaded.edges[0].target, target);
    }

    #[tokio::test]
    async fn test_update_missing_returns_none() {
        // 경합 중 삭제/멱등 재제출 — 없는 문서 update는 에러가 아니라 None이어야 한다.
        let (_tmp, store) = setup().await;
        let out = store
            .update(Uuid::new_v4(), "default", |_| true)
            .await
            .unwrap();
        assert!(out.is_none());
    }

    #[tokio::test]
    async fn test_update_skips_save_when_unchanged() {
        // 클로저가 false를 반환하면 저장을 생략해 불필요한 쓰기를 억제한다.
        use crate::models::{Edge, RelationType};
        let (_tmp, store) = setup().await;
        let doc = make_doc("x");
        let id = doc.id;
        store.save(&doc, "default").await.unwrap();

        store
            .update(id, "default", |d| {
                d.add_edge(Edge::new(Uuid::new_v4(), RelationType::RelatedTo, 0.5));
                false // 변경 없음으로 신고 → 저장 생략
            })
            .await
            .unwrap();

        let loaded = store.load(id, "default").await.unwrap();
        assert!(loaded.edges.is_empty(), "false 반환 시 디스크는 그대로여야 한다");
    }

    #[tokio::test]
    async fn test_update_reads_fresh_state_not_snapshot() {
        // update는 스냅샷이 아니라 최신 디스크 상태를 로드해 수정한다.
        // (감쇠 벌크 스냅샷이 stale 상태를 덮어쓰던 회귀의 핵심 가드.)
        use crate::models::{Edge, RelationType};
        let (_tmp, store) = setup().await;
        let doc = make_doc("x");
        let id = doc.id;
        store.save(&doc, "default").await.unwrap();

        // 앞선 라이터가 엣지 하나 추가.
        store
            .update(id, "default", |d| {
                d.add_edge(Edge::new(Uuid::new_v4(), RelationType::RelatedTo, 0.9));
                true
            })
            .await
            .unwrap();

        // 후속 update의 클로저는 직전 엣지가 보이는 최신 상태를 받아야 한다.
        store
            .update(id, "default", |d| {
                assert_eq!(d.edges.len(), 1, "update는 최신 상태(직전 엣지 포함)를 읽어야 한다");
                d.add_edge(Edge::new(Uuid::new_v4(), RelationType::RelatedTo, 0.9));
                true
            })
            .await
            .unwrap();

        let loaded = store.load(id, "default").await.unwrap();
        assert_eq!(loaded.edges.len(), 2, "두 수정이 모두 누적되어야 한다");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_concurrent_updates_no_lost_edges() {
        // write_lock이 동시 load-modify-save를 직렬화해 서로의 엣지를 덮어쓰지 않아야 한다.
        // (감쇠·add_edge 동시 실행의 엣지 무음 소실 회귀 가드 — "기억을 잃으면 안 된다".)
        use crate::models::{Edge, RelationType};
        use std::sync::Arc;
        let (_tmp, store) = setup().await;
        let store = Arc::new(store);
        let doc = make_doc("shared");
        let id = doc.id;
        store.save(&doc, "default").await.unwrap();

        let n: u128 = 24;
        let mut handles = Vec::new();
        for i in 0..n {
            let s = store.clone();
            handles.push(tokio::spawn(async move {
                let target = Uuid::from_u128(i + 1); // 결정적·상이한 대상
                s.update(id, "default", move |d| {
                    d.add_edge(Edge::new(target, RelationType::RelatedTo, 0.5));
                    true
                })
                .await
                .unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        let loaded = store.load(id, "default").await.unwrap();
        assert_eq!(
            loaded.edges.len(),
            n as usize,
            "동시 수정 {n}건이 모두 반영되어야 한다 (lost-update 없음)"
        );
    }

    // ──── 소스 기반 조회 (커넥터 중복 방지) ────

    fn make_doc_with_source(content: &str, source_id: &str) -> Document {
        use crate::models::DocumentSource;
        make_doc(content).with_source(DocumentSource {
            source_type: "local_directory".to_string(),
            source_id: source_id.to_string(),
            modified_at: chrono::Utc::now(),
            connector_id: "notes".to_string(),
        })
    }

    #[tokio::test]
    async fn test_find_by_source_matches() {
        let (_tmp, store) = setup().await;
        let doc = make_doc_with_source("hello", "/notes/a.md");
        let id = doc.id;
        store.save(&doc, "default").await.unwrap();

        let found = store
            .find_by_source("local_directory", "/notes/a.md", "default")
            .await
            .unwrap();
        assert_eq!(found.map(|d| d.id), Some(id), "일치하는 소스 문서를 찾아야 한다");
    }

    #[tokio::test]
    async fn test_find_by_source_none_when_absent() {
        let (_tmp, store) = setup().await;
        store.save(&make_doc_with_source("x", "/notes/a.md"), "default").await.unwrap();

        // source_id가 다르면 None
        let by_id = store
            .find_by_source("local_directory", "/notes/other.md", "default")
            .await
            .unwrap();
        assert!(by_id.is_none());

        // source_type이 다르면 None (같은 경로여도)
        let by_type = store
            .find_by_source("notion", "/notes/a.md", "default")
            .await
            .unwrap();
        assert!(by_type.is_none());
    }

    #[tokio::test]
    async fn test_find_by_source_ignores_sourceless_docs() {
        // 수동 입력(출처 없음) 문서는 소스 조회에서 무시되어야 한다.
        let (_tmp, store) = setup().await;
        store.save(&make_doc("manual"), "default").await.unwrap();

        let found = store
            .find_by_source("local_directory", "/notes/a.md", "default")
            .await
            .unwrap();
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn test_find_by_source_empty_workspace() {
        let (_tmp, store) = setup().await;
        let found = store
            .find_by_source("local_directory", "/x.md", "empty-ws")
            .await
            .unwrap();
        assert!(found.is_none(), "빈 워크스페이스는 None (에러 아님)");
    }

    #[tokio::test]
    async fn test_find_by_source_workspace_isolation() {
        // 같은 source_id라도 다른 워크스페이스의 문서는 찾지 않는다.
        let (_tmp, store) = setup().await;
        store.save(&make_doc_with_source("a", "/notes/shared.md"), "ws-a").await.unwrap();

        assert!(store
            .find_by_source("local_directory", "/notes/shared.md", "ws-a")
            .await
            .unwrap()
            .is_some());
        assert!(store
            .find_by_source("local_directory", "/notes/shared.md", "ws-b")
            .await
            .unwrap()
            .is_none());
    }

    // ──── 이웃 탐색 (그래프 BFS) ────

    #[tokio::test]
    async fn test_neighbors_one_hop() {
        use crate::models::{Edge, RelationType};
        let (_tmp, store) = setup().await;
        let mut a = make_doc("A");
        let b = make_doc("B");
        a.add_edge(Edge::new(b.id, RelationType::RelatedTo, 0.5));
        store.save(&a, "default").await.unwrap();
        store.save(&b, "default").await.unwrap();

        let neighbors = store.neighbors(a.id, 1, "default").await.unwrap();
        assert_eq!(neighbors.len(), 1);
        assert_eq!(neighbors[0].document.id, b.id);
        assert_eq!(neighbors[0].depth, 1);
        assert_eq!(neighbors[0].relation, RelationType::RelatedTo);
        assert_eq!(neighbors[0].via, a.id);
        assert!((neighbors[0].weight - 0.5).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn test_neighbors_two_hop() {
        // A→B→C, depth=2면 B(1), C(2) 모두 반환하고 경로 정보가 정확해야 한다.
        use crate::models::{Edge, RelationType};
        let (_tmp, store) = setup().await;
        let mut a = make_doc("A");
        let mut b = make_doc("B");
        let c = make_doc("C");
        a.add_edge(Edge::new(b.id, RelationType::RelatedTo, 0.5));
        b.add_edge(Edge::new(c.id, RelationType::PartOf, 0.5));
        store.save(&a, "default").await.unwrap();
        store.save(&b, "default").await.unwrap();
        store.save(&c, "default").await.unwrap();

        let neighbors = store.neighbors(a.id, 2, "default").await.unwrap();
        assert_eq!(neighbors.len(), 2);

        let c_node = neighbors.iter().find(|n| n.document.id == c.id).unwrap();
        assert_eq!(c_node.depth, 2);
        assert_eq!(c_node.via, b.id);
        assert_eq!(c_node.relation, RelationType::PartOf);
    }

    #[tokio::test]
    async fn test_neighbors_depth_limit() {
        // A→B→C, depth=1이면 B만 (C는 상한 밖).
        use crate::models::{Edge, RelationType};
        let (_tmp, store) = setup().await;
        let mut a = make_doc("A");
        let mut b = make_doc("B");
        let c = make_doc("C");
        a.add_edge(Edge::new(b.id, RelationType::RelatedTo, 0.5));
        b.add_edge(Edge::new(c.id, RelationType::RelatedTo, 0.5));
        store.save(&a, "default").await.unwrap();
        store.save(&b, "default").await.unwrap();
        store.save(&c, "default").await.unwrap();

        let neighbors = store.neighbors(a.id, 1, "default").await.unwrap();
        assert_eq!(neighbors.len(), 1);
        assert_eq!(neighbors[0].document.id, b.id);
    }

    #[tokio::test]
    async fn test_neighbors_cycle_safe() {
        // A→B, B→A 순환. depth를 크게 줘도 무한 루프 없이 종료하고 각 문서 1회 방문.
        use crate::models::{Edge, RelationType};
        let (_tmp, store) = setup().await;
        let mut a = make_doc("A");
        let mut b = make_doc("B");
        a.add_edge(Edge::new(b.id, RelationType::RelatedTo, 0.5));
        b.add_edge(Edge::new(a.id, RelationType::RelatedTo, 0.5));
        store.save(&a, "default").await.unwrap();
        store.save(&b, "default").await.unwrap();

        let neighbors = store.neighbors(a.id, MAX_NEIGHBOR_DEPTH, "default").await.unwrap();
        // 시작점 A는 visited라 재방문되지 않고, B만 반환된다.
        assert_eq!(neighbors.len(), 1);
        assert_eq!(neighbors[0].document.id, b.id);
    }

    #[tokio::test]
    async fn test_neighbors_depth_clamped_to_minimum() {
        // depth=0은 최소 1로 클램프되어 직접 이웃을 반환한다.
        use crate::models::{Edge, RelationType};
        let (_tmp, store) = setup().await;
        let mut a = make_doc("A");
        let b = make_doc("B");
        a.add_edge(Edge::new(b.id, RelationType::RelatedTo, 0.5));
        store.save(&a, "default").await.unwrap();
        store.save(&b, "default").await.unwrap();

        let neighbors = store.neighbors(a.id, 0, "default").await.unwrap();
        assert_eq!(neighbors.len(), 1, "depth=0은 1로 클램프되어야 한다");
    }

    #[tokio::test]
    async fn test_neighbors_dangling_edge_skipped() {
        // 대상 문서가 존재하지 않는 엣지는 결과에서 조용히 건너뛴다.
        use crate::models::{Edge, RelationType};
        let (_tmp, store) = setup().await;
        let mut a = make_doc("A");
        a.add_edge(Edge::new(Uuid::new_v4(), RelationType::RelatedTo, 0.5));
        store.save(&a, "default").await.unwrap();

        let neighbors = store.neighbors(a.id, 2, "default").await.unwrap();
        assert!(neighbors.is_empty(), "dangling 엣지는 스킵되어야 한다");
    }

    #[tokio::test]
    async fn test_neighbors_no_edges_empty() {
        let (_tmp, store) = setup().await;
        let a = make_doc("A");
        store.save(&a, "default").await.unwrap();

        let neighbors = store.neighbors(a.id, 2, "default").await.unwrap();
        assert!(neighbors.is_empty());
    }

    #[tokio::test]
    async fn test_neighbors_diamond_visits_once_shortest_depth() {
        // A→B, A→C, B→D, C→D. BFS라 D는 depth=2로 정확히 한 번만 방문된다.
        use crate::models::{Edge, RelationType};
        let (_tmp, store) = setup().await;
        let mut a = make_doc("A");
        let mut b = make_doc("B");
        let mut c = make_doc("C");
        let d = make_doc("D");
        a.add_edge(Edge::new(b.id, RelationType::RelatedTo, 0.5));
        a.add_edge(Edge::new(c.id, RelationType::RelatedTo, 0.5));
        b.add_edge(Edge::new(d.id, RelationType::RelatedTo, 0.5));
        c.add_edge(Edge::new(d.id, RelationType::RelatedTo, 0.5));
        for doc in [&a, &b, &c, &d] {
            store.save(doc, "default").await.unwrap();
        }

        let neighbors = store.neighbors(a.id, 3, "default").await.unwrap();
        assert_eq!(neighbors.len(), 3, "B, C, D 각 한 번씩");
        let d_nodes: Vec<_> = neighbors.iter().filter(|n| n.document.id == d.id).collect();
        assert_eq!(d_nodes.len(), 1, "D는 한 번만 방문되어야 한다");
        assert_eq!(d_nodes[0].depth, 2, "D의 최단 거리는 2");
    }
}
