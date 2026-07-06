//! Patrol 실행 이력 + 마지막 실행 시각.
//!
//! 스케줄러가 "언제 마지막으로 돌았는지"로 다음 실행 시점을 판단하고(due), 소유자가
//! "언제 몇 건 탐지했는지" 이력을 확인한다. 워크스페이스별 단일 파일에 마지막 실행 시각과
//! 최근 실행 목록(상한)을 담는다: `workspaces/{id}/patrol/state.json`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::atomic_write_json;
use super::detectors::{DetectorKind, ReviewCandidate};

/// 이력에 보관할 최근 실행 수 상한(파일 무한 성장 방지).
pub const MAX_HISTORY: usize = 100;

/// 탐지 유형별 건수(이번 실행에서 발견된 후보 기준, dedup 전).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DetectionCounts {
    pub staleness: usize,
    pub duplicate: usize,
    pub orphan: usize,
    pub external_mismatch: usize,
    pub total: usize,
}

impl DetectionCounts {
    /// 탐지 후보 목록에서 유형별 건수를 센다.
    pub fn from_candidates(candidates: &[ReviewCandidate]) -> Self {
        let mut c = DetectionCounts::default();
        for cand in candidates {
            match cand.kind {
                DetectorKind::Staleness => c.staleness += 1,
                DetectorKind::Duplicate => c.duplicate += 1,
                DetectorKind::Orphan => c.orphan += 1,
                DetectorKind::ExternalMismatch => c.external_mismatch += 1,
            }
        }
        c.total = candidates.len();
        c
    }
}

/// 한 번의 Patrol 실행 기록.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PatrolRun {
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    /// 실행 계기: "scheduled" | "manual".
    pub trigger: String,
    /// 유형별 탐지 수(dedup 전 후보 기준).
    pub detections: DetectionCounts,
    /// 큐에 새로 추가된 항목 수(dedup·상한 적용 후).
    pub enqueued: usize,
    /// 자동 재계산으로 가중치가 바뀐 엣지 수.
    pub edges_decayed: usize,
    /// 실패해 격리된 탐지기 유형(관측용). 정상이면 빈 목록.
    #[serde(default)]
    pub failed_detectors: Vec<String>,
}

/// Patrol 상태 — 마지막 실행 시각 + 최근 실행 이력.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PatrolState {
    #[serde(default)]
    pub last_run_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub history: Vec<PatrolRun>,
}

/// 워크스페이스별 Patrol 상태를 파일로 저장/로드한다.
pub struct PatrolHistoryStore {
    data_dir: PathBuf,
    write_lock: Mutex<()>,
}

impl PatrolHistoryStore {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            write_lock: Mutex::new(()),
        }
    }

    fn path(&self, workspace_id: &str) -> PathBuf {
        self.data_dir
            .join("workspaces")
            .join(workspace_id)
            .join("patrol")
            .join("state.json")
    }

    /// 상태를 로드한다(파일 없으면 기본값 = 한 번도 안 돈 상태).
    pub async fn load(&self, workspace_id: &str) -> Result<PatrolState> {
        let path = self.path(workspace_id);
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => serde_json::from_str(&content).context("patrol 상태 파싱 실패"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(PatrolState::default()),
            Err(e) => Err(e).context("patrol 상태 읽기 실패"),
        }
    }

    /// 실행 결과를 기록한다 — last_run_at을 갱신하고 이력 앞에 추가(상한까지 유지).
    pub async fn record(&self, workspace_id: &str, run: PatrolRun) -> Result<()> {
        let _guard = self.write_lock.lock().await;
        let mut state = self.load(workspace_id).await?;
        state.last_run_at = Some(run.finished_at);
        state.history.insert(0, run);
        state.history.truncate(MAX_HISTORY);
        atomic_write_json(&self.path(workspace_id), &state).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;
    use uuid::Uuid;

    fn candidate(kind: DetectorKind) -> ReviewCandidate {
        ReviewCandidate {
            document_id: Uuid::new_v4(),
            kind,
            reason: "r".to_string(),
            evidence: json!({}),
        }
    }

    fn run_at(ts: DateTime<Utc>) -> PatrolRun {
        PatrolRun {
            started_at: ts,
            finished_at: ts,
            trigger: "manual".to_string(),
            detections: DetectionCounts::default(),
            enqueued: 0,
            edges_decayed: 0,
            failed_detectors: vec![],
        }
    }

    #[test]
    fn test_detection_counts_from_candidates() {
        let cands = vec![
            candidate(DetectorKind::Staleness),
            candidate(DetectorKind::Staleness),
            candidate(DetectorKind::Orphan),
            candidate(DetectorKind::ExternalMismatch),
        ];
        let c = DetectionCounts::from_candidates(&cands);
        assert_eq!(c.staleness, 2);
        assert_eq!(c.orphan, 1);
        assert_eq!(c.external_mismatch, 1);
        assert_eq!(c.duplicate, 0);
        assert_eq!(c.total, 4);
    }

    #[tokio::test]
    async fn test_load_missing_default() {
        let tmp = TempDir::new().unwrap();
        let store = PatrolHistoryStore::new(tmp.path());
        let state = store.load("default").await.unwrap();
        assert!(state.last_run_at.is_none());
        assert!(state.history.is_empty());
    }

    #[tokio::test]
    async fn test_record_updates_last_run_and_history() {
        let tmp = TempDir::new().unwrap();
        let store = PatrolHistoryStore::new(tmp.path());
        let ts = Utc::now();
        store.record("default", run_at(ts)).await.unwrap();

        let state = store.load("default").await.unwrap();
        assert_eq!(state.last_run_at, Some(ts));
        assert_eq!(state.history.len(), 1);
    }

    #[tokio::test]
    async fn test_record_prepends_newest_first() {
        let tmp = TempDir::new().unwrap();
        let store = PatrolHistoryStore::new(tmp.path());
        let older = Utc::now() - chrono::Duration::hours(2);
        let newer = Utc::now();
        store.record("default", run_at(older)).await.unwrap();
        store.record("default", run_at(newer)).await.unwrap();

        let state = store.load("default").await.unwrap();
        assert_eq!(state.history.len(), 2);
        assert_eq!(state.history[0].finished_at, newer, "최신이 앞에");
        assert_eq!(state.last_run_at, Some(newer));
    }

    #[tokio::test]
    async fn test_record_caps_history() {
        let tmp = TempDir::new().unwrap();
        let store = PatrolHistoryStore::new(tmp.path());
        for i in 0..(MAX_HISTORY + 10) {
            let ts = Utc::now() + chrono::Duration::seconds(i as i64);
            store.record("default", run_at(ts)).await.unwrap();
        }
        let state = store.load("default").await.unwrap();
        assert_eq!(state.history.len(), MAX_HISTORY, "이력은 상한까지만");
    }

    #[tokio::test]
    async fn test_workspace_isolation() {
        let tmp = TempDir::new().unwrap();
        let store = PatrolHistoryStore::new(tmp.path());
        store.record("personal", run_at(Utc::now())).await.unwrap();
        assert_eq!(store.load("personal").await.unwrap().history.len(), 1);
        assert!(store.load("work").await.unwrap().history.is_empty());
    }
}
