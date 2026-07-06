//! 일 단위 메트릭 롤업 — 검색 품질·그래프 헬스·유입·Patrol 통계.
//!
//! 소유자가 두뇌의 상태를 수치로 관측한다: 검색 zero-result율(지식 공백 신호), 그래프
//! 노드/엣지/고아 수(연결 건강), 유입 전략 분포, Patrol 탐지·처리율. **핵심 경로를 느리게
//! 하지 않도록** 기존 로그(Phase 3 search_log)·상태를 재활용하고 롤업은 배치로 계산한다.
//!
//! 계산 로직은 순수 함수(`compute_*`)로 분리해 mock 없이 단위 테스트한다. 저장은
//! 워크스페이스별 일자 파일(`workspaces/{id}/metrics/{YYYY-MM-DD}.json`).

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::atomic_write_json;
use super::detectors::DocSignal;
use super::review::{ReviewItem, ReviewStatus};
use crate::connectors::sync_state::SyncSummary;
use crate::storage::SearchLogRecord;

/// 검색 품질 지표(그 날의 검색 로그에서 파생).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchMetrics {
    pub count: usize,
    /// 결과 0건 비율(0.0~1.0) — 지식 공백 신호.
    pub zero_result_rate: f32,
    /// 평균 최고 점수.
    pub avg_top_score: f32,
}

/// 그래프 헬스 지표(현재 문서 스냅샷에서 파생).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphMetrics {
    pub nodes: usize,
    pub edges: usize,
    /// 엣지 없는(고립) 문서 수.
    pub orphans: usize,
    /// 노드당 평균 out-degree.
    pub avg_degree: f32,
}

/// 유입 지표(문서 수 + 커넥터 동기화 요약 분포).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IngestMetrics {
    pub document_count: usize,
    /// 유입 전략 분포 — created/updated/skipped/failed 합산(커넥터 요약 기반).
    pub strategy_distribution: HashMap<String, usize>,
}

/// Patrol 지표(이번 실행 탐지 수 + 큐 처리 현황).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PatrolMetrics {
    /// 이번 롤업이 대응하는 Patrol 실행의 탐지 수.
    pub detections: usize,
    /// 미처리(대기) 항목 수.
    pub open_items: usize,
    /// 처리(판단 완료) 항목 수.
    pub resolved_items: usize,
    /// 처리율 = resolved / (open + resolved). 항목이 없으면 0.
    pub resolution_rate: f32,
}

/// 하루치 메트릭 롤업.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DailyRollup {
    /// 일자 키 `YYYY-MM-DD` (사전순 = 시간순).
    pub date: String,
    pub workspace: String,
    pub search: SearchMetrics,
    pub graph: GraphMetrics,
    pub ingest: IngestMetrics,
    pub patrol: PatrolMetrics,
    pub generated_at: DateTime<Utc>,
}

/// 검색 지표 계산(순수). 결과 없으면 0.
pub fn compute_search_metrics(records: &[SearchLogRecord]) -> SearchMetrics {
    let count = records.len();
    if count == 0 {
        return SearchMetrics {
            count: 0,
            zero_result_rate: 0.0,
            avg_top_score: 0.0,
        };
    }
    let zero = records.iter().filter(|r| r.zero_result).count();
    let sum_top: f32 = records.iter().map(|r| r.top_score).sum();
    SearchMetrics {
        count,
        zero_result_rate: zero as f32 / count as f32,
        avg_top_score: sum_top / count as f32,
    }
}

/// 그래프 지표 계산(순수). 노드 없으면 avg_degree=0.
pub fn compute_graph_metrics(signals: &[DocSignal]) -> GraphMetrics {
    let nodes = signals.len();
    let edges: usize = signals.iter().map(|s| s.edge_count).sum();
    let orphans = signals.iter().filter(|s| s.edge_count == 0).count();
    let avg_degree = if nodes == 0 {
        0.0
    } else {
        edges as f32 / nodes as f32
    };
    GraphMetrics {
        nodes,
        edges,
        orphans,
        avg_degree,
    }
}

/// 유입 지표 계산(순수). 커넥터 동기화 요약들을 전략 분포로 합산한다.
pub fn compute_ingest_metrics(document_count: usize, summaries: &[SyncSummary]) -> IngestMetrics {
    let mut dist: HashMap<String, usize> = HashMap::new();
    let mut add = |k: &str, v: usize| {
        if v > 0 {
            *dist.entry(k.to_string()).or_insert(0) += v;
        }
    };
    for s in summaries {
        add("created", s.created);
        add("updated", s.updated);
        add("skipped", s.skipped);
        add("failed", s.failed);
    }
    IngestMetrics {
        document_count,
        strategy_distribution: dist,
    }
}

/// Patrol 지표 계산(순수). `detections`는 이번 실행 탐지 수, `items`는 큐 전체.
pub fn compute_patrol_metrics(detections: usize, items: &[ReviewItem]) -> PatrolMetrics {
    let open = items.iter().filter(|i| i.status == ReviewStatus::Pending).count();
    let resolved = items.len() - open;
    let total = open + resolved;
    let resolution_rate = if total == 0 {
        0.0
    } else {
        resolved as f32 / total as f32
    };
    PatrolMetrics {
        detections,
        open_items: open,
        resolved_items: resolved,
        resolution_rate,
    }
}

/// 워크스페이스별 일자 롤업을 저장/조회한다.
pub struct MetricsStore {
    data_dir: PathBuf,
}

impl MetricsStore {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }

    fn metrics_dir(&self, workspace_id: &str) -> PathBuf {
        self.data_dir
            .join("workspaces")
            .join(workspace_id)
            .join("metrics")
    }

    fn day_path(&self, workspace_id: &str, date: &str) -> PathBuf {
        self.metrics_dir(workspace_id).join(format!("{date}.json"))
    }

    /// 롤업을 일자 파일에 원자적으로 저장한다(같은 날 재실행은 덮어쓰기 = 최신 스냅샷).
    pub async fn save(&self, rollup: &DailyRollup) -> Result<()> {
        atomic_write_json(&self.day_path(&rollup.workspace, &rollup.date), rollup).await
    }

    /// 특정 일자의 롤업을 조회한다(없으면 None).
    pub async fn load(&self, workspace_id: &str, date: &str) -> Result<Option<DailyRollup>> {
        let path = self.day_path(workspace_id, date);
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => Ok(Some(
                serde_json::from_str(&content).context("메트릭 롤업 파싱 실패")?,
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).context("메트릭 롤업 읽기 실패"),
        }
    }

    /// 기간 `[from, until]`(YYYY-MM-DD, 포함)의 롤업을 일자순으로 조회한다.
    /// 파싱 불가한 파일은 건너뛴다(장애 격리).
    pub async fn list_range(
        &self,
        workspace_id: &str,
        from: &str,
        until: &str,
    ) -> Result<Vec<DailyRollup>> {
        let dir = self.metrics_dir(workspace_id);
        let mut entries = match tokio::fs::read_dir(&dir).await {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).context("메트릭 디렉토리 읽기 실패"),
        };

        let mut rollups = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            // 파일명(확장자 제외)이 일자 키. 사전순 비교로 기간 필터.
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if stem < from || stem > until {
                continue;
            }
            if let Ok(content) = tokio::fs::read_to_string(&path).await {
                if let Ok(rollup) = serde_json::from_str::<DailyRollup>(&content) {
                    rollups.push(rollup);
                }
            }
        }
        rollups.sort_by(|a, b| a.date.cmp(&b.date));
        Ok(rollups)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patrol::detectors::DetectorKind;
    use chrono::TimeZone;
    use serde_json::json;
    use tempfile::TempDir;
    use uuid::Uuid;

    fn log_record(zero: bool, top: f32) -> SearchLogRecord {
        SearchLogRecord {
            timestamp: Utc::now(),
            workspace: "default".to_string(),
            query: "q".to_string(),
            mode: "hybrid".to_string(),
            result_count: if zero { 0 } else { 1 },
            top_score: top,
            zero_result: zero,
            duration_ms: 5,
            rounds: None,
        }
    }

    fn doc_signal(edge_count: usize) -> DocSignal {
        DocSignal {
            id: Uuid::new_v4(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            freshness_checked_at: None,
            edge_count,
            summary: "s".to_string(),
            negative_feedback: 0,
            source: None,
        }
    }

    fn review_item(status: ReviewStatus) -> ReviewItem {
        ReviewItem {
            id: Uuid::new_v4(),
            workspace: "default".to_string(),
            document_id: Uuid::new_v4(),
            kind: DetectorKind::Orphan,
            reason: "r".to_string(),
            evidence: json!({}),
            status,
            created_at: Utc::now(),
            decided_at: None,
        }
    }

    fn summary(created: usize, updated: usize, skipped: usize, failed: usize) -> SyncSummary {
        let now = Utc::now();
        SyncSummary {
            started_at: now,
            finished_at: now,
            processed: created + updated + skipped + failed,
            created,
            updated,
            skipped,
            failed,
            failures: vec![],
        }
    }

    // ──── compute_search_metrics ────

    #[test]
    fn test_search_metrics_empty() {
        let m = compute_search_metrics(&[]);
        assert_eq!(m.count, 0);
        assert_eq!(m.zero_result_rate, 0.0);
        assert_eq!(m.avg_top_score, 0.0);
    }

    #[test]
    fn test_search_metrics_zero_rate_and_avg() {
        // 4건 중 1건 zero, 점수 평균.
        let records = vec![
            log_record(false, 0.8),
            log_record(false, 0.6),
            log_record(true, 0.0),
            log_record(false, 0.4),
        ];
        let m = compute_search_metrics(&records);
        assert_eq!(m.count, 4);
        assert!((m.zero_result_rate - 0.25).abs() < 1e-6, "1/4 zero-result");
        assert!((m.avg_top_score - 0.45).abs() < 1e-6, "(0.8+0.6+0+0.4)/4");
    }

    // ──── compute_graph_metrics ────

    #[test]
    fn test_graph_metrics_empty() {
        let m = compute_graph_metrics(&[]);
        assert_eq!(m.nodes, 0);
        assert_eq!(m.edges, 0);
        assert_eq!(m.orphans, 0);
        assert_eq!(m.avg_degree, 0.0);
    }

    #[test]
    fn test_graph_metrics_counts() {
        // 3 노드: degree 2, 0, 1 → edges 3, orphan 1, avg 1.0
        let signals = vec![doc_signal(2), doc_signal(0), doc_signal(1)];
        let m = compute_graph_metrics(&signals);
        assert_eq!(m.nodes, 3);
        assert_eq!(m.edges, 3);
        assert_eq!(m.orphans, 1);
        assert!((m.avg_degree - 1.0).abs() < 1e-6);
    }

    // ──── compute_ingest_metrics ────

    #[test]
    fn test_ingest_metrics_distribution() {
        let summaries = vec![summary(2, 1, 0, 0), summary(1, 0, 3, 1)];
        let m = compute_ingest_metrics(10, &summaries);
        assert_eq!(m.document_count, 10);
        assert_eq!(m.strategy_distribution.get("created"), Some(&3));
        assert_eq!(m.strategy_distribution.get("updated"), Some(&1));
        assert_eq!(m.strategy_distribution.get("skipped"), Some(&3));
        assert_eq!(m.strategy_distribution.get("failed"), Some(&1));
    }

    #[test]
    fn test_ingest_metrics_omits_zero_strategies() {
        let m = compute_ingest_metrics(5, &[summary(2, 0, 0, 0)]);
        assert_eq!(m.strategy_distribution.get("created"), Some(&2));
        assert!(m.strategy_distribution.get("updated").is_none(), "0인 전략은 생략");
    }

    // ──── compute_patrol_metrics ────

    #[test]
    fn test_patrol_metrics_resolution_rate() {
        let items = vec![
            review_item(ReviewStatus::Pending),
            review_item(ReviewStatus::Valid),
            review_item(ReviewStatus::Deleted),
        ];
        let m = compute_patrol_metrics(5, &items);
        assert_eq!(m.detections, 5);
        assert_eq!(m.open_items, 1);
        assert_eq!(m.resolved_items, 2);
        assert!((m.resolution_rate - 2.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn test_patrol_metrics_empty_queue() {
        let m = compute_patrol_metrics(0, &[]);
        assert_eq!(m.resolution_rate, 0.0);
    }

    // ──── MetricsStore ────

    fn rollup(date: &str) -> DailyRollup {
        DailyRollup {
            date: date.to_string(),
            workspace: "default".to_string(),
            search: compute_search_metrics(&[]),
            graph: compute_graph_metrics(&[]),
            ingest: compute_ingest_metrics(0, &[]),
            patrol: compute_patrol_metrics(0, &[]),
            generated_at: Utc.with_ymd_and_hms(2026, 7, 6, 3, 0, 0).unwrap(),
        }
    }

    #[tokio::test]
    async fn test_save_and_load() {
        let tmp = TempDir::new().unwrap();
        let store = MetricsStore::new(tmp.path());
        store.save(&rollup("2026-07-06")).await.unwrap();
        let loaded = store.load("default", "2026-07-06").await.unwrap();
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().date, "2026-07-06");
    }

    #[tokio::test]
    async fn test_load_missing_is_none() {
        let tmp = TempDir::new().unwrap();
        let store = MetricsStore::new(tmp.path());
        assert!(store.load("default", "2026-01-01").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_save_overwrites_same_day() {
        let tmp = TempDir::new().unwrap();
        let store = MetricsStore::new(tmp.path());
        store.save(&rollup("2026-07-06")).await.unwrap();
        store.save(&rollup("2026-07-06")).await.unwrap(); // 재저장
        // 같은 날 파일은 하나여야 한다.
        let range = store.list_range("default", "2026-07-01", "2026-07-31").await.unwrap();
        assert_eq!(range.len(), 1, "같은 날 재실행은 덮어쓰기");
    }

    #[tokio::test]
    async fn test_list_range_filters_and_sorts() {
        let tmp = TempDir::new().unwrap();
        let store = MetricsStore::new(tmp.path());
        for d in ["2026-07-05", "2026-07-06", "2026-07-08", "2026-07-10"] {
            store.save(&rollup(d)).await.unwrap();
        }
        let range = store.list_range("default", "2026-07-06", "2026-07-08").await.unwrap();
        let dates: Vec<&str> = range.iter().map(|r| r.date.as_str()).collect();
        assert_eq!(dates, vec!["2026-07-06", "2026-07-08"], "기간 필터 + 일자순 정렬");
    }

    #[tokio::test]
    async fn test_list_range_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let store = MetricsStore::new(tmp.path());
        assert!(store.list_range("default", "2026-01-01", "2026-12-31").await.unwrap().is_empty());
    }
}
