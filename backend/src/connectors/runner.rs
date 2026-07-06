//! 커넥터 동기화·대량 적재 오케스트레이션.
//!
//! 한 커넥터 인스턴스를 실행해 변경분을 유입한다. 대량 적재는 "커서 없이(full) 도는
//! 동기화"일 뿐이므로 같은 경로를 공유한다.
//!
//! 보장:
//! - **동시성 제한**: `buffer_unordered`로 동시 유입 수를 제한(LLM rate limit 보호).
//! - **진행 관측**: 인메모리 진행 상태를 항목 완료마다 갱신 → 상태 API가 실시간 조회.
//! - **실패 격리**: 개별 항목의 에러·패닉이 전체 실행을 중단시키지 않는다(태스크 spawn으로
//!   패닉을 JoinError로 격리). 실패 목록은 요약에 남는다.
//! - **중단 재개**: 커서는 실패가 없을 때만 전진한다. 중단(상태 미저장)·부분 실패 시 다음
//!   실행이 재스캔하고, 이미 유입된 항목은 소스 dedup으로 `Skipped`되어 이어서 처리된다.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use chrono::Utc;
use futures::stream::StreamExt;
use tokio::sync::RwLock;

use super::sync_state::{SyncFailure, SyncState, SyncStateStore, SyncSummary, MAX_STORED_FAILURES};
use super::{
    build_connector, ConnectorIngest, ConnectorIngestMode, ItemOutcome,
};
use crate::workspace::WorkspaceManager;

/// 실행 옵션.
#[derive(Debug, Clone, Copy)]
pub struct SyncOptions {
    /// 유입 모드 (Parsed/Raw).
    pub mode: ConnectorIngestMode,
    /// true면 커서를 무시하고 전체를 재스캔한다(초기 대량 적재).
    pub full: bool,
    /// 동시성 오버라이드 (None이면 커넥터 설정값).
    pub concurrency: Option<usize>,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            mode: ConnectorIngestMode::Parsed,
            full: false,
            concurrency: None,
        }
    }
}

/// 실행 중/직후의 인메모리 진행 상태 — 상태 API의 실시간 관측 창구.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RunProgress {
    /// 현재 실행 중인지 여부.
    pub running: bool,
    /// 조회된 총 항목 수.
    pub total: usize,
    /// 처리 완료 항목 수 (created+updated+skipped+failed).
    pub processed: usize,
    pub created: usize,
    pub updated: usize,
    pub skipped: usize,
    pub failed: usize,
}

/// 커넥터 실행기. 스케줄러(주기)와 API(수동 트리거)가 공유한다.
pub struct ConnectorRunner {
    workspaces: Arc<WorkspaceManager>,
    ingest: Arc<dyn ConnectorIngest>,
    state: Arc<SyncStateStore>,
    /// 진행 상태 맵 (key = "{workspace}/{connector_id}").
    progress: Arc<RwLock<HashMap<String, RunProgress>>>,
}

impl ConnectorRunner {
    pub fn new(
        workspaces: Arc<WorkspaceManager>,
        ingest: Arc<dyn ConnectorIngest>,
        state: Arc<SyncStateStore>,
    ) -> Self {
        Self {
            workspaces,
            ingest,
            state,
            progress: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    fn progress_key(workspace_id: &str, connector_id: &str) -> String {
        format!("{workspace_id}/{connector_id}")
    }

    /// 현재/직전 실행의 진행 상태를 조회한다.
    pub async fn progress(&self, workspace_id: &str, connector_id: &str) -> Option<RunProgress> {
        self.progress
            .read()
            .await
            .get(&Self::progress_key(workspace_id, connector_id))
            .cloned()
    }

    /// 한 커넥터 인스턴스를 실행한다. 반환 요약은 상태 파일에도 영속화된다.
    ///
    /// 커넥터/워크스페이스가 없거나 설정이 부적절하면 `Err`(실행 자체 불가). 개별 항목의
    /// 실패는 요약의 `failed`/`failures`로 흡수되며 `Err`가 아니다.
    pub async fn run_sync(
        &self,
        workspace_id: &str,
        connector_id: &str,
        opts: SyncOptions,
    ) -> Result<SyncSummary> {
        // 1. 워크스페이스 설정에서 커넥터 인스턴스를 찾는다.
        let config = self
            .workspaces
            .get(workspace_id)
            .await
            .map_err(|e| anyhow!("워크스페이스 '{workspace_id}' 조회 실패: {e}"))?;
        let instance = config
            .connectors
            .iter()
            .find(|c| c.id == connector_id)
            .ok_or_else(|| anyhow!("커넥터 '{connector_id}'를 찾을 수 없습니다"))?
            .clone();

        let concurrency = opts.concurrency.unwrap_or(instance.concurrency).max(1);
        let connector = build_connector(&instance)?;
        // 소스 타입은 커넥터 자신이 보고하는 값을 권위로 삼는다(문서에 각인될 값).
        let source_type = connector.source_type().to_string();

        // 2. 커서 결정 (full이면 무시).
        let prior = self.state.load(workspace_id, connector_id).await?;
        let cursor = if opts.full { None } else { prior.cursor.clone() };

        // 3. 변경분 조회.
        let started_at = Utc::now();
        let fetched = connector.fetch_changes(cursor.as_deref()).await?;
        let total = fetched.items.len();
        tracing::info!(
            "커넥터 '{connector_id}'(ws={workspace_id}) 동기화 시작: {total}건 (mode={:?}, concurrency={concurrency})",
            opts.mode
        );

        // 4. 진행 상태 초기화.
        let key = Self::progress_key(workspace_id, connector_id);
        {
            let mut p = self.progress.write().await;
            p.insert(
                key.clone(),
                RunProgress {
                    running: true,
                    total,
                    ..Default::default()
                },
            );
        }

        // 5. 동시성 제한 유입 — 항목별 태스크 spawn으로 패닉까지 격리.
        let ingest = self.ingest.clone();
        let mode = opts.mode;
        let mut created = 0usize;
        let mut updated = 0usize;
        let mut skipped = 0usize;
        let mut failed = 0usize;
        let mut failures: Vec<SyncFailure> = Vec::new();

        let mut stream = futures::stream::iter(fetched.items.into_iter())
            .map(|item| {
                let ingest = ingest.clone();
                let ws = workspace_id.to_string();
                let st = source_type.clone();
                let cid = connector_id.to_string();
                let source_id = item.source_id.clone();
                async move {
                    // 별도 태스크로 실행해 패닉을 JoinError로 격리한다.
                    let handle = tokio::spawn(async move {
                        ingest.ingest_item(&ws, &st, &cid, item, mode).await
                    });
                    (source_id, handle.await)
                }
            })
            .buffer_unordered(concurrency);

        while let Some((source_id, joined)) = stream.next().await {
            match joined {
                Ok(Ok(ItemOutcome::Created(_))) => created += 1,
                Ok(Ok(ItemOutcome::Updated(_))) => updated += 1,
                Ok(Ok(ItemOutcome::Skipped)) => skipped += 1,
                Ok(Err(e)) => {
                    failed += 1;
                    tracing::warn!("항목 유입 실패(계속): {source_id} — {e}");
                    if failures.len() < MAX_STORED_FAILURES {
                        failures.push(SyncFailure {
                            source_id,
                            error: e.to_string(),
                        });
                    }
                }
                Err(join_err) => {
                    // 유입 태스크 패닉 — 격리하고 계속.
                    failed += 1;
                    tracing::error!("항목 유입 태스크 패닉(격리됨): {source_id} — {join_err}");
                    if failures.len() < MAX_STORED_FAILURES {
                        failures.push(SyncFailure {
                            source_id,
                            error: format!("유입 태스크 패닉(격리됨): {join_err}"),
                        });
                    }
                }
            }

            // 진행 상태 갱신 (실시간 관측).
            let mut p = self.progress.write().await;
            if let Some(rp) = p.get_mut(&key) {
                rp.processed += 1;
                rp.created = created;
                rp.updated = updated;
                rp.skipped = skipped;
                rp.failed = failed;
            }
        }

        let finished_at = Utc::now();
        let summary = SyncSummary {
            started_at,
            finished_at,
            processed: total,
            created,
            updated,
            skipped,
            failed,
            failures,
        };

        // 6. 상태 영속화. 커서는 실패가 없을 때만 전진한다(중단 재개·실패 재시도 보장).
        //    실패가 있으면 이전 커서를 유지해 다음 실행이 실패분을 재스캔하고, 성공분은
        //    소스 dedup으로 Skipped된다(정보 유실 0 지향).
        let next_cursor = if failed == 0 {
            fetched.next_cursor
        } else {
            tracing::warn!(
                "커넥터 '{connector_id}': {failed}건 실패로 커서 미전진(다음 실행에서 재시도)"
            );
            prior.cursor.clone()
        };
        let new_state = SyncState {
            last_run_at: Some(finished_at),
            cursor: next_cursor,
            last_result: Some(summary.clone()),
        };
        self.state.save(workspace_id, connector_id, &new_state).await?;

        // 7. 진행 상태 종료 표시.
        {
            let mut p = self.progress.write().await;
            if let Some(rp) = p.get_mut(&key) {
                rp.running = false;
            }
        }

        tracing::info!(
            "커넥터 '{connector_id}' 동기화 완료: 처리 {total}, 신규 {created}, 갱신 {updated}, 스킵 {skipped}, 실패 {failed}"
        );
        Ok(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::{ConnectorItem, ItemOutcome};
    use crate::workspace::{ConnectorInstance, ConnectorSpec, LocalDirectoryConfig};
    use async_trait::async_trait;
    use std::sync::Mutex;
    use tempfile::TempDir;
    use tokio::fs;
    use uuid::Uuid;

    /// 유입을 흉내내는 mock — 호출을 기록하고 source_id별 동작을 지정한다.
    struct MockIngest {
        calls: Mutex<Vec<String>>,
        /// 이 source_id 접미사를 포함하면 Err.
        fail_on: Option<String>,
        /// 이 source_id 접미사를 포함하면 패닉.
        panic_on: Option<String>,
        /// 모든 항목을 Skipped로.
        skip_all: bool,
    }

    impl MockIngest {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                fail_on: None,
                panic_on: None,
                skip_all: false,
            }
        }
        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl ConnectorIngest for MockIngest {
        async fn ingest_item(
            &self,
            _workspace_id: &str,
            _source_type: &str,
            _connector_id: &str,
            item: ConnectorItem,
            _mode: ConnectorIngestMode,
        ) -> Result<ItemOutcome> {
            self.calls.lock().unwrap().push(item.source_id.clone());
            if let Some(p) = &self.panic_on {
                if item.source_id.contains(p) {
                    panic!("mock 패닉: {}", item.source_id);
                }
            }
            if let Some(f) = &self.fail_on {
                if item.source_id.contains(f) {
                    return Err(anyhow!("mock 실패: {}", item.source_id));
                }
            }
            if self.skip_all {
                return Ok(ItemOutcome::Skipped);
            }
            Ok(ItemOutcome::Created(Uuid::new_v4()))
        }
    }

    /// 워크스페이스 + 로컬 디렉토리 커넥터 + 소스 파일들을 갖춘 픽스처.
    async fn fixture(files: &[(&str, &str)]) -> (TempDir, TempDir, Arc<WorkspaceManager>, Arc<SyncStateStore>) {
        let data = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        for (name, content) in files {
            fs::write(src.path().join(name), content).await.unwrap();
        }

        let workspaces = Arc::new(WorkspaceManager::new(data.path()).await.unwrap());
        workspaces.ensure_default().await.unwrap();

        // default 워크스페이스에 로컬 디렉토리 커넥터 등록
        let mut config = workspaces.get("default").await.unwrap();
        config.connectors.push(ConnectorInstance {
            id: "notes".to_string(),
            enabled: true,
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
        (data, src, workspaces, state)
    }

    #[tokio::test]
    async fn test_run_sync_ingests_all_new_files() {
        let (_data, _src, workspaces, state) =
            fixture(&[("a.md", "A"), ("b.md", "B")]).await;
        let ingest = Arc::new(MockIngest::new());
        let runner = ConnectorRunner::new(workspaces, ingest.clone(), state);

        let summary = runner
            .run_sync("default", "notes", SyncOptions::default())
            .await
            .unwrap();

        assert_eq!(summary.processed, 2);
        assert_eq!(summary.created, 2);
        assert_eq!(summary.failed, 0);
        assert_eq!(ingest.call_count(), 2);
    }

    #[tokio::test]
    async fn test_run_sync_isolates_item_failure() {
        // 한 파일 유입이 실패해도 나머지는 유입되고 실패 목록이 남는다.
        let (_data, _src, workspaces, state) =
            fixture(&[("ok.md", "A"), ("bad.md", "B"), ("ok2.md", "C")]).await;
        let ingest = Arc::new(MockIngest {
            fail_on: Some("bad".to_string()),
            ..MockIngest::new()
        });
        let runner = ConnectorRunner::new(workspaces, ingest, state);

        let summary = runner
            .run_sync("default", "notes", SyncOptions::default())
            .await
            .unwrap();

        assert_eq!(summary.created, 2, "실패해도 나머지는 유입");
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.failures.len(), 1);
        assert!(summary.failures[0].source_id.contains("bad"));
    }

    #[tokio::test]
    async fn test_run_sync_isolates_item_panic() {
        // 유입 태스크 패닉도 격리되어 전체 실행이 완주한다.
        let (_data, _src, workspaces, state) =
            fixture(&[("ok.md", "A"), ("boom.md", "B")]).await;
        let ingest = Arc::new(MockIngest {
            panic_on: Some("boom".to_string()),
            ..MockIngest::new()
        });
        let runner = ConnectorRunner::new(workspaces, ingest, state);

        let summary = runner
            .run_sync("default", "notes", SyncOptions::default())
            .await
            .unwrap();

        assert_eq!(summary.created, 1);
        assert_eq!(summary.failed, 1, "패닉 항목은 실패로 격리");
        assert!(summary.failures[0].error.contains("패닉"));
    }

    #[tokio::test]
    async fn test_cursor_advances_on_full_success_and_incremental_next_run() {
        // 성공 실행 후 커서가 전진하고, 변경 없는 재실행은 0건 처리(증분).
        let (_data, _src, workspaces, state) = fixture(&[("a.md", "A")]).await;
        let ingest = Arc::new(MockIngest::new());
        let runner = ConnectorRunner::new(workspaces, ingest.clone(), state.clone());

        runner.run_sync("default", "notes", SyncOptions::default()).await.unwrap();
        let after = state.load("default", "notes").await.unwrap();
        assert!(after.cursor.is_some(), "성공 후 커서 전진");
        assert!(after.last_run_at.is_some());

        // 파일 변경 없이 재실행 → 커서 이후 수정 없음 → 0건
        let second = runner.run_sync("default", "notes", SyncOptions::default()).await.unwrap();
        assert_eq!(second.processed, 0, "변경 없으면 재유입 안 함(증분)");
        assert_eq!(ingest.call_count(), 1, "두 번째 실행은 유입 호출 없음");
    }

    #[tokio::test]
    async fn test_cursor_not_advanced_on_failure() {
        // 실패가 있으면 커서를 전진시키지 않아 다음 실행이 재시도한다.
        let (_data, _src, workspaces, state) = fixture(&[("bad.md", "A")]).await;
        let ingest = Arc::new(MockIngest {
            fail_on: Some("bad".to_string()),
            ..MockIngest::new()
        });
        let runner = ConnectorRunner::new(workspaces, ingest, state.clone());

        runner.run_sync("default", "notes", SyncOptions::default()).await.unwrap();
        let after = state.load("default", "notes").await.unwrap();
        assert!(after.cursor.is_none(), "실패 시 커서 미전진(재시도 보장)");
        assert!(after.last_result.is_some(), "결과 요약은 기록");
    }

    #[tokio::test]
    async fn test_full_option_ignores_cursor() {
        // full=true면 커서가 있어도 전체 재스캔한다(대량 재적재).
        let (_data, _src, workspaces, state) = fixture(&[("a.md", "A")]).await;
        let ingest = Arc::new(MockIngest { skip_all: true, ..MockIngest::new() });
        let runner = ConnectorRunner::new(workspaces, ingest.clone(), state.clone());

        // 1회차: 커서 전진
        runner.run_sync("default", "notes", SyncOptions::default()).await.unwrap();
        // 2회차 full: 커서 무시하고 다시 조회
        let opts = SyncOptions { full: true, ..SyncOptions::default() };
        let summary = runner.run_sync("default", "notes", opts).await.unwrap();
        assert_eq!(summary.processed, 1, "full은 커서를 무시하고 전체 재스캔");
        assert_eq!(summary.skipped, 1);
    }

    #[tokio::test]
    async fn test_progress_reflects_final_counts() {
        let (_data, _src, workspaces, state) =
            fixture(&[("a.md", "A"), ("b.md", "B")]).await;
        let ingest = Arc::new(MockIngest::new());
        let runner = ConnectorRunner::new(workspaces, ingest, state);

        runner.run_sync("default", "notes", SyncOptions::default()).await.unwrap();
        let progress = runner.progress("default", "notes").await.unwrap();
        assert!(!progress.running, "완료 후 running=false");
        assert_eq!(progress.total, 2);
        assert_eq!(progress.processed, 2);
        assert_eq!(progress.created, 2);
    }

    #[tokio::test]
    async fn test_run_sync_unknown_connector_errors() {
        let (_data, _src, workspaces, state) = fixture(&[("a.md", "A")]).await;
        let ingest = Arc::new(MockIngest::new());
        let runner = ConnectorRunner::new(workspaces, ingest, state);

        let result = runner.run_sync("default", "ghost", SyncOptions::default()).await;
        assert!(result.is_err(), "없는 커넥터는 실행 자체가 Err");
    }
}
