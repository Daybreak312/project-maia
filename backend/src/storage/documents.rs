use anyhow::{Context, Result};
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use tokio::fs;
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
}

impl DocumentStore {
    pub async fn new(data_dir: impl Into<PathBuf>) -> Result<Self> {
        let data_dir = data_dir.into();
        Ok(Self { data_dir })
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
