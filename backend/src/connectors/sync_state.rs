//! 커넥터 동기화 상태 영속화 — 커넥터별 마지막 실행 시각·커서·결과 요약.
//!
//! 경로: `{data_dir}/workspaces/{ws}/connectors/{connector_id}.json`.
//! 커서는 증분 재개의 핵심이다(다음 스캔이 이 지점 이후만 조회). 결과 요약은 상태 조회
//! API·Admin UI가 "마지막 동기화 언제, 신규 몇 건, 실패 몇 건"을 보여주는 근거다.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::fs;

/// 실패 상세를 몇 개까지 보관할지 상한 — 파일 무한 성장 방지.
pub const MAX_STORED_FAILURES: usize = 50;

/// 커넥터 한 인스턴스의 동기화 상태.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SyncState {
    /// 마지막 실행 완료 시각 (한 번도 안 돌았으면 None).
    #[serde(default)]
    pub last_run_at: Option<DateTime<Utc>>,
    /// 다음 증분 조회에 넘길 커서 (커넥터가 해석). None이면 전체 스캔.
    #[serde(default)]
    pub cursor: Option<String>,
    /// 마지막 실행의 결과 요약.
    #[serde(default)]
    pub last_result: Option<SyncSummary>,
}

/// 한 번의 동기화 실행 결과 요약.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncSummary {
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    /// 조회된 총 항목 수 (created + updated + skipped + failed).
    pub processed: usize,
    pub created: usize,
    pub updated: usize,
    pub skipped: usize,
    pub failed: usize,
    /// 실패 상세 (상한까지). 유입 실패가 어느 파일에서 났는지 남긴다.
    #[serde(default)]
    pub failures: Vec<SyncFailure>,
}

/// 한 항목 유입 실패의 기록.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncFailure {
    /// 실패한 소스 항목 식별자 (원본 경로 등).
    pub source_id: String,
    /// 실패 사유.
    pub error: String,
}

/// 워크스페이스별 커넥터 동기화 상태를 파일로 저장/로드한다.
pub struct SyncStateStore {
    data_dir: PathBuf,
}

impl SyncStateStore {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }

    fn state_path(&self, workspace_id: &str, connector_id: &str) -> PathBuf {
        self.data_dir
            .join("workspaces")
            .join(workspace_id)
            .join("connectors")
            .join(format!("{connector_id}.json"))
    }

    /// 상태를 로드한다. 파일이 없으면 기본값(한 번도 안 돈 상태)을 반환한다.
    pub async fn load(&self, workspace_id: &str, connector_id: &str) -> Result<SyncState> {
        let path = self.state_path(workspace_id, connector_id);
        match fs::read_to_string(&path).await {
            Ok(content) => {
                serde_json::from_str(&content).context("커넥터 동기화 상태 파싱 실패")
            }
            // 파일 없음 = 최초 실행 전 → 기본 상태.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(SyncState::default()),
            Err(e) => Err(e).context("커넥터 동기화 상태 읽기 실패"),
        }
    }

    /// 상태를 저장한다(디렉토리 자동 생성).
    pub async fn save(
        &self,
        workspace_id: &str,
        connector_id: &str,
        state: &SyncState,
    ) -> Result<()> {
        let path = self.state_path(workspace_id, connector_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .context("커넥터 상태 디렉토리 생성 실패")?;
        }
        let content = serde_json::to_string_pretty(state).context("커넥터 상태 직렬화 실패")?;
        fs::write(&path, content)
            .await
            .context("커넥터 상태 파일 쓰기 실패")?;
        Ok(())
    }

    /// 커넥터 삭제 시 상태 파일을 정리한다(없으면 무시).
    pub async fn delete(&self, workspace_id: &str, connector_id: &str) -> Result<()> {
        let path = self.state_path(workspace_id, connector_id);
        match fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).context("커넥터 상태 파일 삭제 실패"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn summary() -> SyncSummary {
        let now = Utc::now();
        SyncSummary {
            started_at: now,
            finished_at: now,
            processed: 3,
            created: 2,
            updated: 1,
            skipped: 0,
            failed: 0,
            failures: vec![],
        }
    }

    #[tokio::test]
    async fn test_load_missing_returns_default() {
        let tmp = TempDir::new().unwrap();
        let store = SyncStateStore::new(tmp.path());
        let state = store.load("default", "notes").await.unwrap();
        assert_eq!(state, SyncState::default(), "없는 상태는 기본값");
        assert!(state.last_run_at.is_none());
        assert!(state.cursor.is_none());
    }

    #[tokio::test]
    async fn test_save_and_load_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let store = SyncStateStore::new(tmp.path());
        let state = SyncState {
            last_run_at: Some(Utc::now()),
            cursor: Some("2026-07-06T12:00:00Z".to_string()),
            last_result: Some(summary()),
        };
        store.save("default", "notes", &state).await.unwrap();
        let loaded = store.load("default", "notes").await.unwrap();
        assert_eq!(loaded, state);
    }

    #[tokio::test]
    async fn test_save_creates_connectors_dir() {
        let tmp = TempDir::new().unwrap();
        let store = SyncStateStore::new(tmp.path());
        store
            .save("work", "reports", &SyncState::default())
            .await
            .unwrap();
        assert!(tmp
            .path()
            .join("workspaces/work/connectors/reports.json")
            .exists());
    }

    #[tokio::test]
    async fn test_workspace_and_connector_isolation() {
        let tmp = TempDir::new().unwrap();
        let store = SyncStateStore::new(tmp.path());

        let a = SyncState {
            cursor: Some("cursor-a".to_string()),
            ..Default::default()
        };
        let b = SyncState {
            cursor: Some("cursor-b".to_string()),
            ..Default::default()
        };
        store.save("default", "notes", &a).await.unwrap();
        store.save("default", "reports", &b).await.unwrap();

        assert_eq!(store.load("default", "notes").await.unwrap().cursor.as_deref(), Some("cursor-a"));
        assert_eq!(store.load("default", "reports").await.unwrap().cursor.as_deref(), Some("cursor-b"));
        // 다른 워크스페이스는 격리
        assert!(store.load("work", "notes").await.unwrap().cursor.is_none());
    }

    #[tokio::test]
    async fn test_delete_removes_state() {
        let tmp = TempDir::new().unwrap();
        let store = SyncStateStore::new(tmp.path());
        store.save("default", "notes", &SyncState::default()).await.unwrap();
        assert!(tmp.path().join("workspaces/default/connectors/notes.json").exists());

        store.delete("default", "notes").await.unwrap();
        assert!(!tmp.path().join("workspaces/default/connectors/notes.json").exists());
        // 두 번 삭제해도 에러 없음(멱등)
        store.delete("default", "notes").await.unwrap();
    }

    #[test]
    fn test_summary_roundtrip_with_failures() {
        let mut s = summary();
        s.failed = 1;
        s.failures = vec![SyncFailure {
            source_id: "/notes/broken.md".to_string(),
            error: "boom".to_string(),
        }];
        let json = serde_json::to_string(&s).unwrap();
        let back: SyncSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }
}
