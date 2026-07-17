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

pub mod auto_judge;
pub mod decay;
pub mod detectors;
pub mod feedback;
pub mod freshness;
pub mod metrics;
pub mod review;
pub mod history;
pub mod scheduler;

use std::collections::HashMap;
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
use crate::workspace::{ReviewMode, WorkspaceManager};

use auto_judge::{
    build_auto_judge_prompt, content_excerpt, parse_auto_judgment, AutoJudgeSummary,
    CONTENT_EXCERPT_CHARS,
};
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
use review::{DecidedBy, ReviewDecision, ReviewItem, ReviewQueueStore, ReviewStatus};

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

    /// auto-judge용 자유형식 LLM 응답(워크스페이스 파싱 provider 재사용).
    ///
    /// ingest 판단과 동일한 provider 경로를 쓴다(구독 기반 — 종량제 과금 없음). 실패는
    /// `Err`로 올려, 호출 측이 해당 항목을 Pending에 잔류시키게 한다(침묵 실패 금지).
    async fn patrol_llm_complete(&self, workspace_id: &str, prompt: &str) -> Result<String>;

    /// external_mismatch 해소 — 문서의 커넥터 소스 현재 내용으로 재유입(update)한다.
    ///
    /// 반환: `Ok(true)`면 소스와 정합해짐(재유입했거나 이미 최신) → 항목을 닫아도 안전.
    /// `Ok(false)`면 소스를 확인/재유입할 수 없음(비로컬 소스·파일 없음 등) → Pending 유지.
    /// `Err`는 재유입 시도 중 오류 → Pending 유지. 기존 커넥터 유입 파이프라인을 재사용한다.
    async fn reingest_source_document(&self, workspace_id: &str, id: Uuid) -> Result<bool>;
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
        //    docs는 auto-judge 프롬프트 구성에도 재사용하므로 borrow로 넘긴다(재조회 없음).
        let docs = self.executor.all_documents(workspace_id).await?;
        let signals = self.gather_signals(workspace_id, &docs).await;

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

        // 3.5 auto review 모드: 열린(Pending) 항목 전체를 자동 판정·즉시 반영한다.
        //     실행당 cap 상한으로 LLM 비용을 바운드하고, 실패·불확실은 전부 Pending에 잔류.
        //     삭제는 기존 judge(Deleted) 경로(soft delete)만 재사용한다(하드 삭제 없음).
        let auto_judge = if config.patrol.review_mode == ReviewMode::Auto {
            let summary = self
                .auto_judge_pass(workspace_id, &docs, config.patrol.auto_judge_cap, now)
                .await;
            tracing::info!(
                "auto-judge(ws={}): 시도 {}, 반영 {} (valid {}, needs_fix {}, deleted {}, dismissed {}, 재유입 {}), Pending 잔류 {}",
                workspace_id,
                summary.processed,
                summary.resolved(),
                summary.valid,
                summary.needs_fix,
                summary.deleted,
                summary.dismissed,
                summary.reingested,
                summary.failed
            );
            Some(summary)
        } else {
            None
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
            auto_judge,
        };
        if let Err(e) = self.history.record(workspace_id, run.clone()).await {
            tracing::warn!("Patrol 이력 기록 실패: {}", e);
        }
        Ok(run)
    }

    /// 문서별 DocSignal을 만든다(문서 필드 + 피드백 집계 + freshness 기준점 + 소스 mtime).
    async fn gather_signals(&self, workspace_id: &str, docs: &[Document]) -> Vec<DocSignal> {
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
        // 사람 판단 경로(judge API) — decided_by=human으로 기록한다.
        self.judge_with(workspace_id, item_id, decision, DecidedBy::Human, None, now)
            .await
    }

    /// 판단 공통 경로 — 사람(human)·auto가 함께 쓰며, 판단 주체·근거를 각인한다.
    ///
    /// 부수효과(유효→freshness 갱신, 삭제→복구 가능 soft delete)를 상태 전이 **전에** 수행해
    /// 멱등·크래시 복구 가능성을 유지한다. 부수효과가 실패하면 상태 전이도 일어나지 않아
    /// 항목이 열린 채 남는다(auto 경로에서 이것이 "실패=Pending 잔류" 안전장치의 근거).
    async fn judge_with(
        &self,
        workspace_id: &str,
        item_id: Uuid,
        decision: ReviewDecision,
        decided_by: DecidedBy,
        reason: Option<String>,
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

        let outcome = self
            .reviews
            .judge(workspace_id, item_id, decision, decided_by, reason, now)
            .await?;
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

    // ──── auto review 모드 (review_mode=auto) ────

    /// 열린(Pending) 항목을 자동 판정·즉시 반영한다(실행당 `cap` 상한). `run()`에서만 호출.
    ///
    /// 대상은 이번에 새로 쌓인 것만이 아니라 **열린 항목 전체**다(과거에 사람이 못 본 것도
    /// 처리). 실패·불확실은 전부 Pending에 잔류시켜 정보/판단의 안전성을 코드로 보장한다.
    async fn auto_judge_pass(
        &self,
        workspace_id: &str,
        docs: &[Document],
        cap: usize,
        now: DateTime<Utc>,
    ) -> AutoJudgeSummary {
        let mut summary = AutoJudgeSummary::default();
        if cap == 0 {
            return summary;
        }
        let open = match self
            .reviews
            .list(workspace_id, Some(ReviewStatus::Pending), None)
            .await
        {
            Ok(items) => items,
            Err(e) => {
                tracing::warn!("auto-judge: 열린 항목 로드 실패(스킵): {}", e);
                return summary;
            }
        };
        // docs 스냅샷을 id로 색인해 프롬프트 구성에 재사용(재조회 없음).
        let by_id: HashMap<Uuid, &Document> = docs.iter().map(|d| (d.id, d)).collect();

        for item in open.into_iter().take(cap) {
            summary.processed += 1;
            match item.kind {
                // external_mismatch는 LLM 불필요 — 소스 재유입이 곧 수정.
                DetectorKind::ExternalMismatch => {
                    self.auto_judge_external_mismatch(workspace_id, &item, now, &mut summary)
                        .await;
                }
                // staleness/orphan/duplicate는 LLM 판정.
                _ => {
                    self.auto_judge_llm(workspace_id, &item, &by_id, now, &mut summary)
                        .await;
                }
            }
        }
        summary
    }

    /// external_mismatch 항목: 커넥터 소스 재유입으로 해소를 시도한다(LLM 불필요).
    async fn auto_judge_external_mismatch(
        &self,
        workspace_id: &str,
        item: &ReviewItem,
        now: DateTime<Utc>,
        summary: &mut AutoJudgeSummary,
    ) {
        match self
            .executor
            .reingest_source_document(workspace_id, item.document_id)
            .await
        {
            // 소스와 정합해짐(재유입했거나 이미 최신) → Valid로 닫는다(문서가 지금 최신이므로).
            Ok(true) => {
                if self
                    .apply_auto_decision(
                        workspace_id,
                        item,
                        ReviewDecision::Valid,
                        "소스 재유입으로 해소".to_string(),
                        now,
                    )
                    .await
                {
                    summary.reingested += 1;
                } else {
                    summary.failed += 1;
                }
            }
            Ok(false) => {
                tracing::info!(
                    "auto-judge: external_mismatch 문서 {} 재유입 불가(소스 미지원/미확인) — Pending 유지",
                    item.document_id
                );
                summary.failed += 1;
            }
            Err(e) => {
                tracing::warn!(
                    "auto-judge: external_mismatch 문서 {} 재유입 실패(Pending 유지): {}",
                    item.document_id,
                    e
                );
                summary.failed += 1;
            }
        }
    }

    /// staleness/orphan/duplicate 항목: 대상 문서를 LLM에 판정 요청하고 허용 판정만 반영한다.
    async fn auto_judge_llm(
        &self,
        workspace_id: &str,
        item: &ReviewItem,
        by_id: &HashMap<Uuid, &Document>,
        now: DateTime<Utc>,
        summary: &mut AutoJudgeSummary,
    ) {
        let Some(doc) = by_id.get(&item.document_id) else {
            tracing::warn!(
                "auto-judge: 대상 문서 {} 없음(삭제됨?) — Pending 유지",
                item.document_id
            );
            summary.failed += 1;
            return;
        };
        // duplicate는 상대 문서(evidence.similar_to) 요약도 제공해 무엇을 남길지 판단하게 한다.
        let other_summary = item
            .evidence
            .get("similar_to")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
            .and_then(|id| by_id.get(&id))
            .map(|d| d.summary.as_str());

        let excerpt = content_excerpt(&doc.raw_content, CONTENT_EXCERPT_CHARS);
        let prompt = build_auto_judge_prompt(
            item.kind,
            &item.reason,
            &item.evidence,
            &doc.summary,
            &excerpt,
            other_summary,
        );

        let response = match self.executor.patrol_llm_complete(workspace_id, &prompt).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    "auto-judge: LLM 호출 실패 문서 {}(Pending 유지): {}",
                    item.document_id,
                    e
                );
                summary.failed += 1;
                return;
            }
        };

        match parse_auto_judgment(&response, item.kind) {
            Ok(judgment) => {
                let decision = judgment.decision;
                if self
                    .apply_auto_decision(workspace_id, item, decision, judgment.reason, now)
                    .await
                {
                    match decision {
                        ReviewDecision::Valid => summary.valid += 1,
                        ReviewDecision::NeedsFix => summary.needs_fix += 1,
                        ReviewDecision::Deleted => summary.deleted += 1,
                        ReviewDecision::Dismissed => summary.dismissed += 1,
                    }
                } else {
                    summary.failed += 1;
                }
            }
            // 파싱 실패·허용 목록 밖 판정 → 절대 삭제로 강등하지 않고 Pending 유지.
            Err(e) => {
                tracing::warn!(
                    "auto-judge: 판정 가드 실패 문서 {}(Pending 유지): {}",
                    item.document_id,
                    e
                );
                summary.failed += 1;
            }
        }
    }

    /// auto 판정을 기존 judge 경로(부수효과·멱등)로 반영한다(decided_by=auto·근거 각인).
    /// 성공하면 true. 대상 소멸·부수효과 오류는 warn + false(호출 측이 Pending 잔류로 계상).
    async fn apply_auto_decision(
        &self,
        workspace_id: &str,
        item: &ReviewItem,
        decision: ReviewDecision,
        reason: String,
        now: DateTime<Utc>,
    ) -> bool {
        match self
            .judge_with(
                workspace_id,
                item.id,
                decision,
                DecidedBy::Auto,
                Some(reason),
                now,
            )
            .await
        {
            Ok(Some(_)) => true,
            Ok(None) => {
                tracing::warn!("auto-judge: 판정 대상 항목 {} 사라짐 — 반영 실패", item.id);
                false
            }
            Err(e) => {
                tracing::warn!(
                    "auto-judge: 판정 반영 실패 항목 {}(Pending 유지): {}",
                    item.id,
                    e
                );
                false
            }
        }
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
    use crate::patrol::detectors::ReviewCandidate;
    use crate::workspace::{ReviewMode, WorkspaceManager};
    use review::ReviewStatus;
    use serde_json::json;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;
    use tokio::sync::Mutex;

    /// 문서 실행기 mock — 반환 문서를 지정하고 삭제·감쇠 호출을 관측하며, auto-judge용
    /// LLM 응답·소스 재유입 결과를 미리 채워 판정 분기를 결정적으로 고정한다(Qdrant·실 LLM 무관).
    struct MockExecutor {
        docs: Vec<Document>,
        deleted: Mutex<Vec<Uuid>>,
        decay_calls: AtomicUsize,
        /// auto-judge LLM 응답 큐(앞에서부터 소비). 소진되면 Err(테스트 오구성 노출).
        llm_responses: Mutex<VecDeque<Result<String>>>,
        /// external_mismatch 재유입 결과 큐(앞에서부터 소비). 소진되면 Ok(false)(미해소).
        reingest_results: Mutex<VecDeque<Result<bool>>>,
        llm_calls: AtomicUsize,
        reingest_calls: AtomicUsize,
    }
    impl MockExecutor {
        fn new(docs: Vec<Document>) -> Self {
            Self {
                docs,
                deleted: Mutex::new(Vec::new()),
                decay_calls: AtomicUsize::new(0),
                llm_responses: Mutex::new(VecDeque::new()),
                reingest_results: Mutex::new(VecDeque::new()),
                llm_calls: AtomicUsize::new(0),
                reingest_calls: AtomicUsize::new(0),
            }
        }
        /// auto-judge LLM 응답을 미리 채운다(빌더).
        fn with_llm(mut self, responses: Vec<Result<String>>) -> Self {
            self.llm_responses = Mutex::new(responses.into_iter().collect());
            self
        }
        /// external_mismatch 재유입 결과를 미리 채운다(빌더).
        fn with_reingest(mut self, results: Vec<Result<bool>>) -> Self {
            self.reingest_results = Mutex::new(results.into_iter().collect());
            self
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
        async fn patrol_llm_complete(&self, _ws: &str, _prompt: &str) -> Result<String> {
            self.llm_calls.fetch_add(1, Ordering::SeqCst);
            self.llm_responses
                .lock()
                .await
                .pop_front()
                .unwrap_or_else(|| Err(anyhow::anyhow!("mock LLM 응답 소진")))
        }
        async fn reingest_source_document(&self, _ws: &str, _id: Uuid) -> Result<bool> {
            self.reingest_calls.fetch_add(1, Ordering::SeqCst);
            self.reingest_results
                .lock()
                .await
                .pop_front()
                .unwrap_or(Ok(false))
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
        fixture_inner(Arc::new(MockExecutor::new(docs)), None).await
    }

    /// review_mode=auto로 전환한 fixture(auto_judge_cap=cap). LLM/재유입 응답은 executor에 미리 채운다.
    async fn auto_fixture(
        executor: Arc<MockExecutor>,
        cap: usize,
    ) -> (TempDir, Patrol, Arc<MockExecutor>) {
        fixture_inner(executor, Some(cap)).await
    }

    async fn fixture_inner(
        executor: Arc<MockExecutor>,
        auto_cap: Option<usize>,
    ) -> (TempDir, Patrol, Arc<MockExecutor>) {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path();
        let workspaces = Arc::new(WorkspaceManager::new(path).await.unwrap());
        workspaces.ensure_default().await.unwrap();
        if let Some(cap) = auto_cap {
            // default 워크스페이스를 auto 모드로 전환(운영 설정 적용과 동형).
            let mut cfg = workspaces.get("default").await.unwrap();
            cfg.patrol.review_mode = ReviewMode::Auto;
            cfg.patrol.auto_judge_cap = cap;
            workspaces.update("default", cfg).await.unwrap();
        }
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

    // ──── auto review 모드 ────

    /// 고유 요약으로 stale 문서를 만든다(같은 요약 다수는 duplicate로도 잡혀 격리를 깬다).
    fn old_doc_with_summary(days: i64, summary: &str) -> Document {
        let mut d = Document::new("raw".to_string(), summary.to_string(), vec![], vec![]);
        d.updated_at = Utc::now() - chrono::Duration::days(days);
        d.created_at = Utc::now() - chrono::Duration::days(days);
        d.add_edge(Edge::new(Uuid::new_v4(), RelationType::RelatedTo, 0.5));
        d
    }

    #[tokio::test]
    async fn test_auto_mode_deletes_staleness_via_llm() {
        // review_mode=auto: 탐지→enqueue→LLM 판정(deleted)→즉시 soft delete + decided_by=auto.
        let executor = Arc::new(MockExecutor::new(vec![old_doc(400)]).with_llm(vec![Ok(
            r#"{"decision":"deleted","reason":"1년 이상 미사용·대체 존재"}"#.to_string(),
        )]));
        let (_t, patrol, executor) = auto_fixture(executor, 50).await;
        let now = Utc::now();

        let run = patrol.run("default", "manual", now).await.unwrap();
        let summary = run.auto_judge.expect("auto 모드는 요약을 기록해야 한다");
        assert_eq!(summary.processed, 1);
        assert_eq!(summary.deleted, 1, "LLM 판정대로 삭제 반영");
        assert_eq!(summary.failed, 0);

        // soft delete 경로가 실제로 호출됐다(하드 삭제 아님).
        assert_eq!(executor.deleted.lock().await.len(), 1, "복구 가능 삭제 위임");
        // 큐 항목이 Deleted + 판단 주체/근거 각인.
        let deleted = patrol
            .list_reviews("default", Some(ReviewStatus::Deleted), None)
            .await
            .unwrap();
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0].decided_by, Some(DecidedBy::Auto));
        assert!(deleted[0].decision_reason.is_some(), "판단 근거 기록");
        // 열린 항목이 남지 않는다.
        assert_eq!(
            patrol.list_reviews("default", Some(ReviewStatus::Pending), None).await.unwrap().len(),
            0
        );
    }

    #[tokio::test]
    async fn test_auto_mode_llm_failure_keeps_pending_never_deletes() {
        // 안전 불변식: LLM 응답이 쓰레기여도 절대 삭제로 강등하지 않고 Pending에 잔류시킨다.
        let executor = Arc::new(
            MockExecutor::new(vec![old_doc(400)]).with_llm(vec![Ok("완전 쓰레기 응답".to_string())]),
        );
        let (_t, patrol, executor) = auto_fixture(executor, 50).await;
        let now = Utc::now();

        let run = patrol.run("default", "manual", now).await.unwrap();
        let summary = run.auto_judge.unwrap();
        assert_eq!(summary.processed, 1);
        assert_eq!(summary.failed, 1, "파싱 실패는 실패로 계상");
        assert_eq!(summary.deleted, 0);
        assert!(executor.deleted.lock().await.is_empty(), "실패는 삭제로 강등되지 않는다");
        assert_eq!(
            patrol.list_reviews("default", Some(ReviewStatus::Pending), None).await.unwrap().len(),
            1,
            "실패 항목은 Pending 잔류"
        );
    }

    #[tokio::test]
    async fn test_auto_mode_out_of_allowlist_decision_keeps_pending() {
        // duplicate에 valid는 허용 목록 밖 → 거부 → Pending 유지(범위 밖 판정 봉인).
        let a = old_doc_with_summary(2, "리액트 훅과 상태 관리 완전 정리 노트");
        let mut b = old_doc_with_summary(1, "리액트 훅과 상태 관리 완전 정리 노트");
        b.created_at = Utc::now(); // b가 더 최근 → duplicate로 b가 플래그됨
        let executor = Arc::new(
            MockExecutor::new(vec![a, b]).with_llm(vec![Ok(
                r#"{"decision":"valid","reason":"둘 다 유지하고 싶음"}"#.to_string(),
            )]),
        );
        let (_t, patrol, executor) = auto_fixture(executor, 50).await;
        let now = Utc::now();

        let run = patrol.run("default", "manual", now).await.unwrap();
        let summary = run.auto_judge.unwrap();
        assert!(summary.deleted == 0 && summary.valid == 0, "허용 밖 판정은 미반영");
        assert_eq!(summary.failed, 1, "범위 밖 판정은 실패(Pending 유지)");
        assert!(executor.deleted.lock().await.is_empty());
    }

    #[tokio::test]
    async fn test_auto_mode_respects_cap() {
        // cap=1이면 열린 항목이 여럿이어도 실행당 1건만 판정한다(LLM 비용 바운드).
        let docs = vec![
            old_doc_with_summary(400, "리액트 훅 정리"),
            old_doc_with_summary(400, "쿠버네티스 배포 파이프라인"),
            old_doc_with_summary(400, "A사 면접 후기 기록"),
        ];
        let executor = Arc::new(MockExecutor::new(docs).with_llm(vec![Ok(
            r#"{"decision":"valid","reason":"유효"}"#.to_string(),
        )]));
        let (_t, patrol, executor) = auto_fixture(executor, 1).await;
        let now = Utc::now();

        let run = patrol.run("default", "manual", now).await.unwrap();
        assert_eq!(run.detections.staleness, 3, "3건 stale 탐지");
        let summary = run.auto_judge.unwrap();
        assert_eq!(summary.processed, 1, "cap=1 → 1건만 처리");
        assert_eq!(executor.llm_calls.load(Ordering::SeqCst), 1, "LLM도 1회만 호출");
        assert_eq!(
            patrol.list_reviews("default", Some(ReviewStatus::Pending), None).await.unwrap().len(),
            2,
            "나머지 2건은 다음 실행으로 이월(Pending)"
        );
    }

    #[tokio::test]
    async fn test_manual_mode_skips_auto_judge() {
        // 기본(manual)에서는 auto-judge가 돌지 않는다(요약 None, LLM 미호출).
        let (_t, patrol, executor) = fixture(vec![old_doc(400)]).await;
        let now = Utc::now();
        let run = patrol.run("default", "manual", now).await.unwrap();
        assert!(run.auto_judge.is_none(), "manual 모드는 auto-judge 미실행");
        assert_eq!(executor.llm_calls.load(Ordering::SeqCst), 0, "manual은 LLM 미호출");
        assert_eq!(
            patrol.list_reviews("default", Some(ReviewStatus::Pending), None).await.unwrap().len(),
            1,
            "manual은 사람 판단 대기로 남긴다"
        );
    }

    #[tokio::test]
    async fn test_auto_judge_external_mismatch_reingest_resolves() {
        // external_mismatch: 소스 재유입 성공 → Valid로 닫힘("소스 재유입으로 해소"), LLM 불호출.
        let doc = old_doc(5);
        let doc_id = doc.id;
        let executor =
            Arc::new(MockExecutor::new(vec![doc.clone()]).with_reingest(vec![Ok(true)]));
        let (_t, patrol, executor) = auto_fixture(executor, 50).await;
        let now = Utc::now();

        // external_mismatch 항목을 직접 큐에 넣는다(탐지기는 실 파일 stat이 필요해 단위 테스트 회피).
        let cand = ReviewCandidate {
            document_id: doc_id,
            kind: DetectorKind::ExternalMismatch,
            reason: "소스가 유입 이후 수정됨".to_string(),
            evidence: json!({"source_id": "/notes/a.md"}),
        };
        patrol.reviews.enqueue("default", &[cand], now, 50).await.unwrap();

        let summary = patrol.auto_judge_pass("default", &[doc], 50, now).await;
        assert_eq!(summary.reingested, 1, "재유입으로 해소");
        assert_eq!(summary.failed, 0);
        assert_eq!(executor.reingest_calls.load(Ordering::SeqCst), 1);
        assert_eq!(executor.llm_calls.load(Ordering::SeqCst), 0, "external_mismatch는 LLM 불필요");

        let items = patrol
            .list_reviews("default", None, Some(DetectorKind::ExternalMismatch))
            .await
            .unwrap();
        assert_eq!(items[0].status, ReviewStatus::Valid);
        assert_eq!(items[0].decided_by, Some(DecidedBy::Auto));
        assert_eq!(items[0].decision_reason.as_deref(), Some("소스 재유입으로 해소"));
    }

    #[tokio::test]
    async fn test_auto_judge_external_mismatch_unresolved_keeps_pending() {
        // 재유입 불가(비로컬 소스 등, Ok(false)) → 항목은 Pending에 잔류한다.
        let doc = old_doc(5);
        let doc_id = doc.id;
        let executor =
            Arc::new(MockExecutor::new(vec![doc.clone()]).with_reingest(vec![Ok(false)]));
        let (_t, patrol, _e) = auto_fixture(executor, 50).await;
        let now = Utc::now();

        let cand = ReviewCandidate {
            document_id: doc_id,
            kind: DetectorKind::ExternalMismatch,
            reason: "소스가 유입 이후 수정됨".to_string(),
            evidence: json!({"source_id": "s3://bucket/key"}),
        };
        patrol.reviews.enqueue("default", &[cand], now, 50).await.unwrap();

        let summary = patrol.auto_judge_pass("default", &[doc], 50, now).await;
        assert_eq!(summary.reingested, 0);
        assert_eq!(summary.failed, 1, "재유입 불가는 Pending 잔류(failed로 계상)");
        assert_eq!(
            patrol.list_reviews("default", Some(ReviewStatus::Pending), None).await.unwrap().len(),
            1
        );
    }
}
