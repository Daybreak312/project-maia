//! Patrol — 자기 관리 & 메모리 거버넌스 (Phase 5).
//!
//! 두뇌가 스스로의 기억 상태를 점검하는 **반자율** 계층. 청소부가 아니라 관측·거버넌스다:
//! 시스템이 후보를 식별해 플래그를 세우고(Review Queue), 소유자가 판단하고, 그 피드백이
//! 축적된다. Patrol 자체는 **읽기 + 플래그 + 감쇠 재계산**만 하며 문서를 변경/삭제하지 않는다.
//!
//! 구조:
//! - [`decay`]: 엣지 시간 감쇠(수학적 유지보수, 사람 판단 불필요). 순수·멱등.
//! - [`detectors`]: staleness/중복/고아/외부 불일치 탐지기(LLM 없이 수치 신호 기반, 순수).
//! - [`review`]: Review Queue 모델·저장·중복 방지·판단 처리.
//! - [`freshness`]: "유효" 판단 시각 기준점(staleness 유예의 근거).
//! - [`feedback`]: 검색 "관련 없음" 피드백 수집(일 JSONL) + doc별 집계(staleness 신호).
//! - [`metrics`]: 일 단위 메트릭 롤업(검색/그래프/유입/Patrol) 순수 계산·저장.
//! - [`history`]: Patrol 실행 이력·마지막 실행 시각(스케줄 판단).
//! - [`scheduler`]: 주기 실행 + 오류 격리.

pub mod decay;
pub mod detectors;
pub mod feedback;
pub mod freshness;
pub mod metrics;
pub mod review;
pub mod history;
pub mod scheduler;

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::connectors::sync_state::{SyncStateStore, SyncSummary};
use crate::models::{Document, DocumentSource};
use crate::storage::SearchLogStore;
use crate::workspace::WorkspaceManager;

use detectors::{
    combine_detector_results, detect_duplicates, detect_external_mismatch, detect_orphans,
    detect_stale, DetectorKind, DocSignal, SourceSignal, Thresholds,
};
use feedback::{FeedbackKind, FeedbackRecord, FeedbackStore};
use freshness::FreshnessStore;
use history::{DetectionCounts, PatrolHistoryStore, PatrolRun, PatrolState};
use metrics::{
    compute_graph_metrics, compute_ingest_metrics, compute_patrol_metrics, compute_search_metrics,
    DailyRollup, MetricsStore,
};
use review::{ReviewDecision, ReviewItem, ReviewQueueStore, ReviewStatus};

/// 유형별 한 번의 Patrol 실행에서 생성할 Review 항목 상한(오탐 폭주 방어).
/// Patrol이 대체로 주기당 1회 도므로 사실상 유형별 일일 상한이다.
pub const DEFAULT_PER_TYPE_CAP: usize = 50;

/// JSON을 파일에 **원자적으로** 쓴다 — temp에 쓴 뒤 rename으로 교체(torn write → 부팅
/// 브릭/큐 유실 방지). 호출 측이 write_lock으로 직렬화하므로 고정 temp 이름이 안전하다.
pub(crate) async fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .context("patrol 저장 디렉토리 생성 실패")?;
    }
    let content = serde_json::to_string_pretty(value).context("patrol 상태 직렬화 실패")?;
    let tmp = path.with_extension("tmp");
    tokio::fs::write(&tmp, content)
        .await
        .context("patrol temp 파일 쓰기 실패")?;
    tokio::fs::rename(&tmp, path)
        .await
        .context("patrol 원자적 교체(rename) 실패")?;
    Ok(())
}

/// Patrol이 문서 계층에 요구하는 실행 계약 — `Indexer`가 구현한다.
///
/// 러너/스케줄러 패턴과 동일하게, 단위 테스트는 이 trait을 mock으로 대체해 Qdrant·LLM
/// 없이 오케스트레이션(신호 수집·탐지·enqueue·판단·감쇠)을 검증한다.
#[async_trait]
pub trait PatrolExecutor: Send + Sync {
    /// 워크스페이스 raw 문서 전체(신호 수집용).
    async fn all_documents(&self, workspace_id: &str) -> Result<Vec<Document>>;
    /// 복구 가능한 삭제(버전 보관 후 삭제, 멱등).
    async fn soft_delete_document(&self, workspace_id: &str, id: Uuid) -> Result<()>;
    /// 워크스페이스 엣지 시간 감쇠 자동 재계산. 바뀐 엣지 수 반환.
    async fn decay_workspace_edges(
        &self,
        workspace_id: &str,
        lambda: f32,
        now: DateTime<Utc>,
    ) -> Result<usize>;
}

/// 현재 소스의 실제 수정 시각을 확인한다(외부 불일치 신호).
///
/// 로컬 디렉토리 커넥터는 `source_id`가 파일 경로라 stat으로 현재 mtime을 얻는다. 다른
/// 소스 타입은 현재 수정 시각을 알 방법이 없어 None을 반환하고(판단 보류), 파일이
/// 옮겨졌거나 접근 불가여도 None이다(오탐 방지). 향후 커넥터별로 확장할 지점.
async fn resolve_current_source_mtime(source: &DocumentSource) -> Option<DateTime<Utc>> {
    if source.source_type != "local_directory" {
        return None;
    }
    let meta = tokio::fs::metadata(&source.source_id).await.ok()?;
    let modified = meta.modified().ok()?;
    Some(DateTime::<Utc>::from(modified))
}

/// Patrol 오케스트레이터 — 자기 관리 루프의 파사드.
///
/// 실행(run): 신호 수집 → 탐지기 4종 격리 실행 → Review Queue enqueue(dedup·상한) →
/// 엣지 감쇠 자동 재계산 → 메트릭 롤업 → 이력 기록. 판단(judge): 유효→freshness 갱신,
/// 삭제→복구 가능 삭제. Patrol 자체는 문서 내용을 변경/삭제하지 않으며, 삭제는 오직
/// 사람 판단(judge)에서만 일어난다.
pub struct Patrol {
    executor: Arc<dyn PatrolExecutor>,
    workspaces: Arc<WorkspaceManager>,
    search_logs: Arc<SearchLogStore>,
    sync_state: Arc<SyncStateStore>,
    reviews: Arc<ReviewQueueStore>,
    feedback: Arc<FeedbackStore>,
    freshness: Arc<FreshnessStore>,
    metrics: Arc<MetricsStore>,
    history: Arc<PatrolHistoryStore>,
}

impl Patrol {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        executor: Arc<dyn PatrolExecutor>,
        workspaces: Arc<WorkspaceManager>,
        search_logs: Arc<SearchLogStore>,
        sync_state: Arc<SyncStateStore>,
        reviews: Arc<ReviewQueueStore>,
        feedback: Arc<FeedbackStore>,
        freshness: Arc<FreshnessStore>,
        metrics: Arc<MetricsStore>,
        history: Arc<PatrolHistoryStore>,
    ) -> Self {
        Self {
            executor,
            workspaces,
            search_logs,
            sync_state,
            reviews,
            feedback,
            freshness,
            metrics,
            history,
        }
    }

    /// 워크스페이스 Patrol 1회 실행. `trigger`는 "scheduled"|"manual". 실행 리포트를 반환.
    ///
    /// 개별 부수 단계(enqueue·감쇠·메트릭·이력)의 실패는 격리해 warn만 하고 실행을 완주한다
    /// — 하나가 실패해도 나머지 관측·유지보수는 수행된다(관측 가능한 부분 성공).
    pub async fn run(&self, workspace_id: &str, trigger: &str, now: DateTime<Utc>) -> Result<PatrolRun> {
        let config = self.workspaces.get(workspace_id).await?;
        let thresholds = Thresholds::from_strictness(config.patrol.strictness);

        // 1. 신호 수집 — 문서 전체 + 피드백 + freshness + 소스 mtime.
        let docs = self.executor.all_documents(workspace_id).await?;
        let signals = self.gather_signals(workspace_id, docs).await;

        // 2. 탐지기 4종 — 독립 실행·실패 격리(combine이 실패한 탐지기만 건너뜀).
        //    탐지기는 순수·무오류지만, 격리 계약을 아키텍처로 고정한다(향후 오류형 탐지기 대비).
        let results = vec![
            (DetectorKind::Staleness, Ok(detect_stale(&signals, now, &thresholds))),
            (
                DetectorKind::Duplicate,
                Ok(detect_duplicates(&signals, &thresholds, DEFAULT_PER_TYPE_CAP)),
            ),
            (DetectorKind::Orphan, Ok(detect_orphans(&signals, now, &thresholds))),
            (DetectorKind::ExternalMismatch, Ok(detect_external_mismatch(&signals))),
        ];
        let (candidates, failed) = combine_detector_results(results);
        let detections = DetectionCounts::from_candidates(&candidates);

        // 3. Review Queue enqueue — 열린 동일 항목 dedup + 유형별 상한.
        let enqueued = match self
            .reviews
            .enqueue(workspace_id, &candidates, now, DEFAULT_PER_TYPE_CAP)
            .await
        {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("Review Queue enqueue 실패(다음 실행이 재탐지): {}", e);
                0
            }
        };

        // 4. 엣지 감쇠 자동 재계산(Phase 2 lambda 재사용, 판단 불필요한 수학적 유지보수).
        let edges_decayed = match self
            .executor
            .decay_workspace_edges(workspace_id, config.search.time_decay_lambda, now)
            .await
        {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("엣지 감쇠 재계산 실패: {}", e);
                0
            }
        };

        // 5. 메트릭 롤업(best-effort — 관측용).
        self.rollup_metrics(workspace_id, now, &signals, &candidates).await;

        // 6. 이력 기록.
        let run = PatrolRun {
            started_at: now,
            finished_at: now,
            trigger: trigger.to_string(),
            detections,
            enqueued,
            edges_decayed,
            failed_detectors: failed.iter().map(|k| k.as_str().to_string()).collect(),
        };
        if let Err(e) = self.history.record(workspace_id, run.clone()).await {
            tracing::warn!("Patrol 이력 기록 실패: {}", e);
        }
        Ok(run)
    }

    /// 문서별 DocSignal을 만든다(문서 필드 + 피드백 집계 + freshness 기준점 + 소스 mtime).
    async fn gather_signals(&self, workspace_id: &str, docs: Vec<Document>) -> Vec<DocSignal> {
        let freshness = match self.freshness.load(workspace_id).await {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!("freshness 로드 실패(빈 맵으로 진행): {}", e);
                Default::default()
            }
        };
        let feedback = match self.feedback.count_negative_by_doc(workspace_id).await {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!("피드백 집계 실패(0으로 진행): {}", e);
                Default::default()
            }
        };

        let mut signals = Vec::with_capacity(docs.len());
        for doc in docs {
            let source = match &doc.source {
                Some(src) => Some(SourceSignal {
                    source_id: src.source_id.clone(),
                    recorded_modified_at: src.modified_at,
                    current_modified_at: resolve_current_source_mtime(src).await,
                }),
                None => None,
            };
            signals.push(DocSignal {
                id: doc.id,
                created_at: doc.created_at,
                updated_at: doc.updated_at,
                freshness_checked_at: freshness.get(&doc.id).copied(),
                edge_count: doc.edges.len(),
                summary: doc.summary.clone(),
                negative_feedback: feedback.get(&doc.id).copied().unwrap_or(0),
                source,
            });
        }
        signals
    }

    /// 일자 메트릭 롤업을 계산·저장한다(best-effort — 실패해도 patrol은 성공).
    async fn rollup_metrics(
        &self,
        workspace_id: &str,
        now: DateTime<Utc>,
        signals: &[DocSignal],
        candidates: &[detectors::ReviewCandidate],
    ) {
        let date = now.format("%Y-%m-%d").to_string();
        let search_records = self
            .search_logs
            .read_day(workspace_id, &date)
            .await
            .unwrap_or_default();
        let search = compute_search_metrics(&search_records);
        let graph = compute_graph_metrics(signals);
        let summaries = self.connector_summaries(workspace_id).await;
        let ingest = compute_ingest_metrics(signals.len(), &summaries);
        let items = self.reviews.load(workspace_id).await.unwrap_or_default();
        let patrol = compute_patrol_metrics(candidates.len(), &items);

        let rollup = DailyRollup {
            date,
            workspace: workspace_id.to_string(),
            search,
            graph,
            ingest,
            patrol,
            generated_at: now,
        };
        if let Err(e) = self.metrics.save(&rollup).await {
            tracing::warn!("메트릭 롤업 저장 실패(무해): {}", e);
        }
    }

    /// 워크스페이스 커넥터들의 마지막 동기화 요약을 모은다(유입 전략 분포용).
    async fn connector_summaries(&self, workspace_id: &str) -> Vec<SyncSummary> {
        let Ok(config) = self.workspaces.get(workspace_id).await else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for connector in &config.connectors {
            if let Ok(state) = self.sync_state.load(workspace_id, &connector.id).await {
                if let Some(summary) = state.last_result {
                    out.push(summary);
                }
            }
        }
        out
    }

    /// 단일 항목 판단 — 부수효과를 조율한다. 없으면 None(404).
    ///
    /// 유효 → freshness 기준점 갱신(유예), 삭제 → 복구 가능 삭제, 그 외 → 상태만. 부수효과를
    /// 상태 전이 **전에** 수행해, 전이 후 크래시가 나도 재제출로 복구되게 한다(부수효과는
    /// 멱등: freshness touch, soft-delete 모두 재실행 안전). 이미 판단된 항목은 부수효과
    /// 없이 그대로 반환한다(멱등 no-op).
    pub async fn judge(
        &self,
        workspace_id: &str,
        item_id: Uuid,
        decision: ReviewDecision,
        now: DateTime<Utc>,
    ) -> Result<Option<ReviewItem>> {
        let items = self.reviews.load(workspace_id).await?;
        let Some(item) = items.iter().find(|i| i.id == item_id) else {
            return Ok(None);
        };
        if !item.status.is_open() {
            return Ok(Some(item.clone())); // 이미 판단됨 — 멱등 no-op
        }
        let doc_id = item.document_id;

        match decision {
            ReviewDecision::Valid => self.freshness.touch(workspace_id, doc_id, now).await?,
            ReviewDecision::Deleted => {
                self.executor.soft_delete_document(workspace_id, doc_id).await?
            }
            ReviewDecision::NeedsFix | ReviewDecision::Dismissed => {}
        }

        let outcome = self.reviews.judge(workspace_id, item_id, decision, now).await?;
        Ok(outcome.map(|o| o.item))
    }

    /// 여러 항목 일괄 판단(각 항목 부수효과 조율). 존재한 항목들의 결과를 반환.
    pub async fn judge_bulk(
        &self,
        workspace_id: &str,
        item_ids: &[Uuid],
        decision: ReviewDecision,
        now: DateTime<Utc>,
    ) -> Result<Vec<ReviewItem>> {
        let mut out = Vec::new();
        for id in item_ids {
            if let Some(item) = self.judge(workspace_id, *id, decision, now).await? {
                out.push(item);
            }
        }
        Ok(out)
    }

    // ──── 조회 파사드(API가 쓰는 읽기 경로) ────

    /// Review Queue 조회(상태·유형 필터).
    pub async fn list_reviews(
        &self,
        workspace_id: &str,
        status: Option<ReviewStatus>,
        kind: Option<DetectorKind>,
    ) -> Result<Vec<ReviewItem>> {
        self.reviews.list(workspace_id, status, kind).await
    }

    /// Patrol 실행 이력·마지막 실행 시각.
    pub async fn history(&self, workspace_id: &str) -> Result<PatrolState> {
        self.history.load(workspace_id).await
    }

    /// 기간 메트릭 롤업 조회.
    pub async fn metrics_range(
        &self,
        workspace_id: &str,
        from: &str,
        until: &str,
    ) -> Result<Vec<DailyRollup>> {
        self.metrics.list_range(workspace_id, from, until).await
    }

    /// 검색 "관련 없음" 피드백을 수집한다(실패 무해).
    pub async fn record_feedback(
        &self,
        workspace_id: &str,
        query: String,
        document_id: Uuid,
        now: DateTime<Utc>,
    ) {
        self.feedback
            .append_best_effort(FeedbackRecord {
                timestamp: now,
                workspace: workspace_id.to_string(),
                query,
                document_id,
                kind: FeedbackKind::NotRelevant,
            })
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Document, Edge, RelationType};
    use crate::workspace::WorkspaceManager;
    use review::ReviewStatus;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;
    use tokio::sync::Mutex;

    /// 문서 실행기 mock — 반환 문서를 지정하고, 삭제·감쇠 호출을 관측한다.
    struct MockExecutor {
        docs: Vec<Document>,
        deleted: Mutex<Vec<Uuid>>,
        decay_calls: AtomicUsize,
    }
    impl MockExecutor {
        fn new(docs: Vec<Document>) -> Self {
            Self {
                docs,
                deleted: Mutex::new(Vec::new()),
                decay_calls: AtomicUsize::new(0),
            }
        }
    }
    #[async_trait]
    impl PatrolExecutor for MockExecutor {
        async fn all_documents(&self, _ws: &str) -> Result<Vec<Document>> {
            Ok(self.docs.clone())
        }
        async fn soft_delete_document(&self, _ws: &str, id: Uuid) -> Result<()> {
            self.deleted.lock().await.push(id);
            Ok(())
        }
        async fn decay_workspace_edges(
            &self,
            _ws: &str,
            _lambda: f32,
            _now: DateTime<Utc>,
        ) -> Result<usize> {
            self.decay_calls.fetch_add(1, Ordering::SeqCst);
            Ok(3)
        }
    }

    fn old_doc(days: i64) -> Document {
        let mut d = Document::new(
            "raw".to_string(),
            "요약".to_string(),
            vec![],
            vec![],
        );
        d.updated_at = Utc::now() - chrono::Duration::days(days);
        d.created_at = Utc::now() - chrono::Duration::days(days);
        // 엣지 하나 부여 — 고아로 잡히지 않게(staleness만 격리 테스트).
        d.add_edge(Edge::new(Uuid::new_v4(), RelationType::RelatedTo, 0.5));
        d
    }

    async fn fixture(docs: Vec<Document>) -> (TempDir, Patrol, Arc<MockExecutor>) {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path();
        let workspaces = Arc::new(WorkspaceManager::new(path).await.unwrap());
        workspaces.ensure_default().await.unwrap();
        let executor = Arc::new(MockExecutor::new(docs));
        let patrol = Patrol::new(
            executor.clone(),
            workspaces,
            Arc::new(SearchLogStore::new(path)),
            Arc::new(SyncStateStore::new(path)),
            Arc::new(ReviewQueueStore::new(path)),
            Arc::new(FeedbackStore::new(path)),
            Arc::new(FreshnessStore::new(path)),
            Arc::new(MetricsStore::new(path)),
            Arc::new(PatrolHistoryStore::new(path)),
        );
        (tmp, patrol, executor)
    }

    #[tokio::test]
    async fn test_run_enqueues_staleness_and_records_history() {
        // 오래된 문서(300일) → staleness 후보 → 큐에 1건 + 이력 기록 + 감쇠 호출.
        let (_t, patrol, executor) = fixture(vec![old_doc(300)]).await;
        let now = Utc::now();
        let run = patrol.run("default", "manual", now).await.unwrap();

        assert_eq!(run.detections.staleness, 1, "staleness 1건 탐지");
        assert_eq!(run.enqueued, 1, "큐에 1건 추가");
        assert_eq!(executor.decay_calls.load(Ordering::SeqCst), 1, "감쇠 재계산 호출");

        let open = patrol
            .list_reviews("default", Some(ReviewStatus::Pending), None)
            .await
            .unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].kind, DetectorKind::Staleness);

        // 이력에 기록되었는가.
        let state = patrol.history("default").await.unwrap();
        assert_eq!(state.history.len(), 1);
        assert!(state.last_run_at.is_some());
        assert_eq!(state.history[0].trigger, "manual");
    }

    #[tokio::test]
    async fn test_run_dedups_across_runs() {
        // 같은 문서를 두 번 실행해도 열린 staleness 항목은 중복 생성되지 않는다.
        let (_t, patrol, _e) = fixture(vec![old_doc(300)]).await;
        let now = Utc::now();
        patrol.run("default", "scheduled", now).await.unwrap();
        let second = patrol.run("default", "scheduled", now).await.unwrap();
        assert_eq!(second.enqueued, 0, "이미 열린 항목은 재추가 안 됨");
        assert_eq!(
            patrol.list_reviews("default", Some(ReviewStatus::Pending), None).await.unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn test_run_writes_metric_rollup() {
        let (_t, patrol, _e) = fixture(vec![old_doc(300)]).await;
        let now = Utc::now();
        patrol.run("default", "manual", now).await.unwrap();
        let date = now.format("%Y-%m-%d").to_string();
        let range = patrol.metrics_range("default", &date, &date).await.unwrap();
        assert_eq!(range.len(), 1, "일자 롤업이 생성되어야 한다");
        assert_eq!(range[0].graph.nodes, 1);
        assert_eq!(range[0].patrol.detections, 1);
    }

    #[tokio::test]
    async fn test_judge_valid_touches_freshness_and_suppresses() {
        // 유효 판단 → freshness 갱신 → 재실행 시 staleness 유예.
        let (_t, patrol, _e) = fixture(vec![old_doc(300)]).await;
        let now = Utc::now();
        patrol.run("default", "manual", now).await.unwrap();
        let item = patrol
            .list_reviews("default", Some(ReviewStatus::Pending), None)
            .await
            .unwrap()[0]
            .clone();

        let judged = patrol
            .judge("default", item.id, ReviewDecision::Valid, now)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(judged.status, ReviewStatus::Valid);

        // 재실행: freshness가 최근이라 staleness가 다시 잡히지 않는다.
        let rerun = patrol.run("default", "manual", now).await.unwrap();
        assert_eq!(rerun.detections.staleness, 0, "유효 판단 후 staleness 유예");
    }

    #[tokio::test]
    async fn test_judge_deleted_soft_deletes_document() {
        let (_t, patrol, executor) = fixture(vec![old_doc(300)]).await;
        let now = Utc::now();
        patrol.run("default", "manual", now).await.unwrap();
        let item = patrol
            .list_reviews("default", Some(ReviewStatus::Pending), None)
            .await
            .unwrap()[0]
            .clone();

        patrol
            .judge("default", item.id, ReviewDecision::Deleted, now)
            .await
            .unwrap();

        let deleted = executor.deleted.lock().await;
        assert_eq!(deleted.len(), 1, "문서 삭제가 실행기에 위임됨");
        assert_eq!(deleted[0], item.document_id);
    }

    #[tokio::test]
    async fn test_judge_missing_returns_none() {
        let (_t, patrol, _e) = fixture(vec![]).await;
        let res = patrol
            .judge("default", Uuid::new_v4(), ReviewDecision::Valid, Utc::now())
            .await
            .unwrap();
        assert!(res.is_none());
    }

    #[tokio::test]
    async fn test_judge_idempotent_no_double_delete() {
        // 삭제 판단 재제출은 부수효과를 두 번 실행하지 않는다(멱등).
        let (_t, patrol, executor) = fixture(vec![old_doc(300)]).await;
        let now = Utc::now();
        patrol.run("default", "manual", now).await.unwrap();
        let item = patrol
            .list_reviews("default", Some(ReviewStatus::Pending), None)
            .await
            .unwrap()[0]
            .clone();

        patrol.judge("default", item.id, ReviewDecision::Deleted, now).await.unwrap();
        patrol.judge("default", item.id, ReviewDecision::Deleted, now).await.unwrap(); // 재제출

        assert_eq!(executor.deleted.lock().await.len(), 1, "삭제 부수효과는 1회뿐");
    }

    #[tokio::test]
    async fn test_record_feedback_feeds_staleness() {
        // 피드백이 유효 나이를 밀어올려, 나이만으로는 임계 미만인 문서를 staleness로 만든다.
        // strictness 0.3 → 임계 ≈ 264.5일, 피드백 30일/건.
        let mut doc = old_doc(250); // 250 < 264.5 → 나이만으론 미탐지
        // 확실히 하기 위해 엣지 유지(고아 아님), 요약 유지.
        doc.updated_at = Utc::now() - chrono::Duration::days(250);
        let doc_id = doc.id;
        let (_t, patrol, _e) = fixture(vec![doc]).await;
        let now = Utc::now();

        // 피드백 없으면 미탐지.
        let first = patrol.run("default", "manual", now).await.unwrap();
        assert_eq!(first.detections.staleness, 0, "나이만으론 임계 미만");

        // 피드백 1건(+30일) → 280 ≥ 264.5 → 탐지.
        patrol.record_feedback("default", "무관 쿼리".to_string(), doc_id, now).await;
        let second = patrol.run("default", "manual", now).await.unwrap();
        assert_eq!(second.detections.staleness, 1, "피드백이 노화를 가속해 탐지");
    }
}
