//! 커넥터 스케줄러 — 워크스페이스별 커넥터를 설정 주기로 자동 실행한다.
//!
//! 설계:
//! - **단일 틱 루프**: 고정 간격으로 깨어나 매 틱마다 모든 워크스페이스의 커넥터를 훑고,
//!   "마지막 실행 + 주기 <= now"인 활성 커넥터를 실행한다. 커넥터별 태스크를 따로
//!   관리하지 않아 설정 변경(등록/삭제)이 다음 틱에 자연히 반영된다.
//! - **오류 격리**: 각 커넥터 실행을 태스크로 spawn해 패닉을 JoinError로 격리하고, 어떤
//!   실행의 실패·패닉도 스케줄러 루프나 서버 프로세스를 중단시키지 않는다.
//! - **기동 시 자동 시작**: tokio interval의 첫 틱은 즉시 발생하므로, 기동 직후 한 번
//!   순회한다(due한 커넥터 즉시 실행).

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};

use super::runner::{ConnectorRunner, SyncOptions};
use super::sync_state::SyncStateStore;
use crate::workspace::WorkspaceManager;

/// 스케줄러 틱 간격 기본값(초). 주기 도래를 이 해상도로 감지한다.
pub const DEFAULT_TICK_SECS: u64 = 30;

/// 커넥터가 실행 대상인지 판단하는 순수 함수.
///
/// 한 번도 안 돌았으면(`last_run=None`) 즉시 대상. 아니면 마지막 실행 이후 경과가 주기
/// 이상이면 대상이다.
pub fn due(last_run: Option<DateTime<Utc>>, interval_secs: u64, now: DateTime<Utc>) -> bool {
    match last_run {
        None => true,
        Some(last) => now.signed_duration_since(last).num_seconds() >= interval_secs as i64,
    }
}

pub struct ConnectorScheduler {
    runner: Arc<ConnectorRunner>,
    workspaces: Arc<WorkspaceManager>,
    state: Arc<SyncStateStore>,
    tick_interval: Duration,
}

impl ConnectorScheduler {
    pub fn new(
        runner: Arc<ConnectorRunner>,
        workspaces: Arc<WorkspaceManager>,
        state: Arc<SyncStateStore>,
    ) -> Self {
        Self {
            runner,
            workspaces,
            state,
            tick_interval: Duration::from_secs(DEFAULT_TICK_SECS),
        }
    }

    /// 백그라운드 루프를 시작한다. 서버 요청 경로와 격리된 태스크로 돈다.
    pub fn start(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(self.tick_interval);
            // 실행 지연이 누적돼도 틱이 몰아치지 않도록 Skip 전략.
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            tracing::info!("커넥터 스케줄러 시작 (틱 {}s)", self.tick_interval.as_secs());
            loop {
                ticker.tick().await;
                self.tick_once(Utc::now()).await;
            }
        });
    }

    /// 한 틱: 모든 워크스페이스의 due한 활성 커넥터를 실행한다. 실행 수를 반환한다.
    ///
    /// 각 실행은 태스크로 격리되어, 하나의 실패·패닉이 나머지나 루프를 중단시키지 않는다.
    pub async fn tick_once(&self, now: DateTime<Utc>) -> usize {
        let workspaces = self.workspaces.list().await;
        let mut ran = 0usize;

        for ws in workspaces {
            for connector in &ws.connectors {
                if !connector.enabled {
                    continue;
                }
                let state = match self.state.load(&ws.id, &connector.id).await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(
                            "커넥터 '{}'(ws={}) 상태 로드 실패(이번 틱 스킵): {e}",
                            connector.id,
                            ws.id
                        );
                        continue;
                    }
                };
                if !due(state.last_run_at, connector.interval_secs, now) {
                    continue;
                }

                ran += 1;
                // 태스크로 격리 — 실행의 패닉이 스케줄러/서버를 죽이지 않는다.
                let runner = self.runner.clone();
                let ws_id = ws.id.clone();
                let connector_id = connector.id.clone();
                let handle = tokio::spawn(async move {
                    runner
                        .run_sync(&ws_id, &connector_id, SyncOptions::default())
                        .await
                });
                match handle.await {
                    Ok(Ok(summary)) => {
                        tracing::info!(
                            "스케줄 동기화 완료 '{}'(ws={}): 신규 {}, 갱신 {}, 실패 {}",
                            connector.id,
                            ws.id,
                            summary.created,
                            summary.updated,
                            summary.failed
                        );
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(
                            "스케줄 동기화 오류 '{}'(ws={}) (격리됨): {e}",
                            connector.id,
                            ws.id
                        );
                    }
                    Err(join_err) => {
                        tracing::error!(
                            "스케줄 동기화 태스크 패닉 '{}'(ws={}) (격리됨): {join_err}",
                            connector.id,
                            ws.id
                        );
                    }
                }
            }
        }

        ran
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::{ConnectorIngest, ConnectorIngestMode, ConnectorItem, ItemOutcome};
    use crate::workspace::{ConnectorInstance, ConnectorSpec, LocalDirectoryConfig, WorkspaceManager};
    use anyhow::Result;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;
    use tokio::fs;
    use uuid::Uuid;

    // ──── 순수 함수 due ────

    #[test]
    fn test_due_never_run_is_immediate() {
        assert!(due(None, 3600, Utc::now()));
    }

    #[test]
    fn test_due_elapsed_past_interval() {
        let now = Utc::now();
        let last = now - chrono::Duration::seconds(3601);
        assert!(due(Some(last), 3600, now), "주기 초과 → 대상");
    }

    #[test]
    fn test_not_due_within_interval() {
        let now = Utc::now();
        let last = now - chrono::Duration::seconds(100);
        assert!(!due(Some(last), 3600, now), "주기 미도래 → 비대상");
    }

    #[test]
    fn test_due_exactly_at_interval() {
        let now = Utc::now();
        let last = now - chrono::Duration::seconds(3600);
        assert!(due(Some(last), 3600, now), "정확히 주기 경과 → 대상");
    }

    // ──── tick_once ────

    /// 호출 횟수를 세는 mock. 패닉 옵션으로 격리를 검증한다.
    struct CountingIngest {
        count: AtomicUsize,
        panic_always: bool,
    }
    impl CountingIngest {
        fn new(panic_always: bool) -> Self {
            Self {
                count: AtomicUsize::new(0),
                panic_always,
            }
        }
    }
    #[async_trait]
    impl ConnectorIngest for CountingIngest {
        async fn ingest_item(
            &self,
            _ws: &str,
            _st: &str,
            _cid: &str,
            _item: ConnectorItem,
            _mode: ConnectorIngestMode,
        ) -> Result<ItemOutcome> {
            self.count.fetch_add(1, Ordering::SeqCst);
            if self.panic_always {
                panic!("mock 패닉");
            }
            Ok(ItemOutcome::Created(Uuid::new_v4()))
        }
    }

    async fn scheduler_fixture(
        enabled: bool,
        panic_always: bool,
    ) -> (TempDir, TempDir, Arc<ConnectorScheduler>, Arc<CountingIngest>, Arc<SyncStateStore>) {
        let data = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        fs::write(src.path().join("a.md"), "A").await.unwrap();

        let workspaces = Arc::new(WorkspaceManager::new(data.path()).await.unwrap());
        workspaces.ensure_default().await.unwrap();
        let mut config = workspaces.get("default").await.unwrap();
        config.connectors.push(ConnectorInstance {
            id: "notes".to_string(),
            enabled,
            interval_secs: 3600,
            concurrency: 2,
            spec: ConnectorSpec::LocalDirectory(LocalDirectoryConfig {
                directories: vec![src.path().to_string_lossy().into_owned()],
                extensions: vec!["md".to_string()],
                exclude: vec![],
                max_file_bytes: 1_048_576,
            }),
        });
        workspaces.update("default", config).await.unwrap();

        let state = Arc::new(SyncStateStore::new(data.path()));
        let ingest = Arc::new(CountingIngest::new(panic_always));
        let runner = Arc::new(ConnectorRunner::new(
            workspaces.clone(),
            ingest.clone(),
            state.clone(),
        ));
        let scheduler = Arc::new(ConnectorScheduler::new(runner, workspaces, state.clone()));
        (data, src, scheduler, ingest, state)
    }

    #[tokio::test]
    async fn test_tick_runs_due_connector() {
        let (_d, _s, scheduler, ingest, state) = scheduler_fixture(true, false).await;
        let ran = scheduler.tick_once(Utc::now()).await;
        assert_eq!(ran, 1, "due한 활성 커넥터 1개 실행");
        assert_eq!(ingest.count.load(Ordering::SeqCst), 1, "파일 1개 유입");
        // 실행 후 상태에 last_run 기록
        assert!(state.load("default", "notes").await.unwrap().last_run_at.is_some());
    }

    #[tokio::test]
    async fn test_tick_skips_disabled_connector() {
        let (_d, _s, scheduler, ingest, _state) = scheduler_fixture(false, false).await;
        let ran = scheduler.tick_once(Utc::now()).await;
        assert_eq!(ran, 0, "비활성 커넥터는 실행 안 함");
        assert_eq!(ingest.count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn test_tick_skips_not_due_after_run() {
        let (_d, _s, scheduler, ingest, _state) = scheduler_fixture(true, false).await;
        // 1회차 실행 → last_run 기록
        scheduler.tick_once(Utc::now()).await;
        // 곧바로 다시 틱 → 주기(3600s) 미도래로 스킵
        let ran = scheduler.tick_once(Utc::now()).await;
        assert_eq!(ran, 0, "주기 미도래면 재실행 안 함");
        assert_eq!(ingest.count.load(Ordering::SeqCst), 1, "유입은 1회뿐");
    }

    #[tokio::test]
    async fn test_tick_survives_run_panic() {
        // 유입이 항상 패닉해도 tick_once는 정상 반환한다(격리). 서버 안 죽음.
        let (_d, _s, scheduler, ingest, _state) = scheduler_fixture(true, true).await;
        let ran = scheduler.tick_once(Utc::now()).await;
        assert_eq!(ran, 1, "실행은 시도됨");
        assert!(ingest.count.load(Ordering::SeqCst) >= 1, "유입이 호출되고 패닉했지만 격리됨");
        // tick_once가 패닉하지 않고 여기까지 도달한 것 자체가 격리 증명.
    }
}
