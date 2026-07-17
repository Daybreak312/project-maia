//! Patrol 스케줄러 — 워크스페이스별 patrol을 설정 주기로 자동 실행한다.
//!
//! ConnectorScheduler와 동일한 설계: 고정 틱 루프로 깨어나 각 워크스페이스의
//! "마지막 실행 + 주기 <= now"인 것을 실행하고, 각 실행을 태스크로 격리해 실패·패닉이
//! 루프나 서버를 죽이지 않게 한다. 주기 도래 판정(`due`)은 커넥터 스케줄러의 순수 함수를
//! 재사용한다. patrol은 저빈도(일/주 단위)라 틱 간격을 시간 단위로 크게 잡는다.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};

use super::history::PatrolHistoryStore;
use super::Patrol;
use crate::connectors::scheduler::due;
use crate::workspace::WorkspaceManager;

/// 스케줄러 틱 간격 기본값(초). patrol은 저빈도라 시간 단위로 깨어난다.
pub const DEFAULT_TICK_SECS: u64 = 3600;

/// patrol frequency 문자열 → 주기(초).
///
/// 인식 형식:
/// - 키워드: "hourly"/"daily"/"weekly".
/// - `<숫자><단위>` 스펙: 단위는 `h`(시간)·`d`(일). 예: "6h"→21600, "12h", "2d".
///
/// 그 외(빈 문자열·cron 표현식·알 수 없는 단위·0/음수)는 보수적으로 daily로 폴백한다
/// (cron 파싱은 이 phase 범위 밖 — 과잉 설계 방지). 순수 함수라 mock 없이 검증된다.
pub fn patrol_interval_secs(frequency: &str) -> u64 {
    let norm = frequency.trim().to_lowercase();
    match norm.as_str() {
        "hourly" => return 3_600,
        "daily" => return 86_400,
        "weekly" => return 604_800,
        _ => {}
    }
    // "<n>h" / "<n>d" 커스텀 주기(예: "6h" → 6시간).
    parse_duration_spec(&norm).unwrap_or(86_400)
}

/// `<양의 정수><h|d>` 스펙을 초로 파싱한다(순수). 형식이 아니면 None(호출 측이 daily 폴백).
///
/// 방어: 단위 없음/숫자 없음/0/파싱 불가/오버플로는 모두 None. h는 3600, d는 86400 배수.
fn parse_duration_spec(s: &str) -> Option<u64> {
    let (digits, mult) = if let Some(d) = s.strip_suffix('h') {
        (d, 3_600u64)
    } else if let Some(d) = s.strip_suffix('d') {
        (d, 86_400u64)
    } else {
        return None;
    };
    let n: u64 = digits.parse().ok()?;
    if n == 0 {
        return None; // 0h/0d는 무의미 → daily 폴백
    }
    n.checked_mul(mult)
}

pub struct PatrolScheduler {
    patrol: Arc<Patrol>,
    workspaces: Arc<WorkspaceManager>,
    history: Arc<PatrolHistoryStore>,
    tick_interval: Duration,
}

impl PatrolScheduler {
    pub fn new(
        patrol: Arc<Patrol>,
        workspaces: Arc<WorkspaceManager>,
        history: Arc<PatrolHistoryStore>,
    ) -> Self {
        Self {
            patrol,
            workspaces,
            history,
            tick_interval: Duration::from_secs(DEFAULT_TICK_SECS),
        }
    }

    /// 백그라운드 루프를 시작한다(요청 경로와 격리된 태스크). 기동 시 첫 틱 즉시 발생.
    pub fn start(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(self.tick_interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            tracing::info!("Patrol 스케줄러 시작 (틱 {}s)", self.tick_interval.as_secs());
            loop {
                ticker.tick().await;
                self.tick_once(Utc::now()).await;
            }
        });
    }

    /// 한 틱: 모든 워크스페이스의 due한 patrol을 실행한다. 실행 수를 반환.
    ///
    /// 각 실행을 태스크로 격리해, 하나의 실패·패닉이 나머지나 루프를 중단시키지 않는다.
    pub async fn tick_once(&self, now: DateTime<Utc>) -> usize {
        let workspaces = self.workspaces.list().await;
        let mut ran = 0usize;

        for ws in workspaces {
            let interval = patrol_interval_secs(&ws.patrol.frequency);
            let state = match self.history.load(&ws.id).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("Patrol 상태 로드 실패(ws={}, 이번 틱 스킵): {}", ws.id, e);
                    continue;
                }
            };
            if !due(state.last_run_at, interval, now) {
                continue;
            }

            ran += 1;
            let patrol = self.patrol.clone();
            let ws_id = ws.id.clone();
            let handle = tokio::spawn(async move { patrol.run(&ws_id, "scheduled", Utc::now()).await });
            match handle.await {
                Ok(Ok(run)) => tracing::info!(
                    "스케줄 Patrol 완료(ws={}): 탐지 {}, 신규 {}, 감쇠 {}",
                    ws.id,
                    run.detections.total,
                    run.enqueued,
                    run.edges_decayed
                ),
                Ok(Err(e)) => tracing::warn!("스케줄 Patrol 오류(ws={}, 격리됨): {}", ws.id, e),
                Err(join_err) => {
                    tracing::error!("스케줄 Patrol 태스크 패닉(ws={}, 격리됨): {}", ws.id, join_err)
                }
            }
        }
        ran
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::sync_state::SyncStateStore;
    use crate::models::Document;
    use crate::patrol::feedback::FeedbackStore;
    use crate::patrol::freshness::FreshnessStore;
    use crate::patrol::metrics::MetricsStore;
    use crate::patrol::review::ReviewQueueStore;
    use crate::patrol::PatrolExecutor;
    use crate::storage::SearchLogStore;
    use crate::workspace::WorkspaceManager;
    use anyhow::Result;
    use async_trait::async_trait;
    use tempfile::TempDir;
    use uuid::Uuid;

    // ──── patrol_interval_secs (순수) ────

    #[test]
    fn test_interval_mapping() {
        assert_eq!(patrol_interval_secs("hourly"), 3_600);
        assert_eq!(patrol_interval_secs("daily"), 86_400);
        assert_eq!(patrol_interval_secs("weekly"), 604_800);
        // 알 수 없는 값·cron은 daily 기본.
        assert_eq!(patrol_interval_secs("0 0 * * *"), 86_400);
        assert_eq!(patrol_interval_secs(""), 86_400);
        // 대소문자·공백 관대.
        assert_eq!(patrol_interval_secs(" Weekly "), 604_800);
    }

    #[test]
    fn test_interval_custom_hour_day_spec() {
        // "<n>h" / "<n>d" 커스텀 주기 — 6시간 주기가 이 phase의 핵심 요구.
        assert_eq!(patrol_interval_secs("6h"), 21_600);
        assert_eq!(patrol_interval_secs("12h"), 43_200);
        assert_eq!(patrol_interval_secs("1h"), 3_600, "1h는 hourly와 등가");
        assert_eq!(patrol_interval_secs("24h"), 86_400, "24h는 daily와 등가");
        assert_eq!(patrol_interval_secs("2d"), 172_800);
        // 대소문자·공백 관대(설정 값이 " 6H "로 들어와도 동작).
        assert_eq!(patrol_interval_secs(" 6H "), 21_600);
    }

    #[test]
    fn test_interval_custom_spec_invalid_falls_back_daily() {
        // 방어: 0/단위없음/숫자없음/알 수 없는 단위/음수는 daily 폴백(브릭 방지).
        assert_eq!(patrol_interval_secs("0h"), 86_400, "0h는 무의미 → daily");
        assert_eq!(patrol_interval_secs("0d"), 86_400);
        assert_eq!(patrol_interval_secs("h"), 86_400, "숫자 없음 → daily");
        assert_eq!(patrol_interval_secs("6"), 86_400, "단위 없음 → daily");
        assert_eq!(patrol_interval_secs("6x"), 86_400, "알 수 없는 단위 → daily");
        assert_eq!(patrol_interval_secs("-6h"), 86_400, "음수 → daily");
        assert_eq!(patrol_interval_secs("6.5h"), 86_400, "소수 → daily");
    }

    // ──── tick_once (스케줄) ────

    /// 아무 문서도 없는 실행기(스케줄 판정만 검증).
    struct EmptyExecutor;
    #[async_trait]
    impl PatrolExecutor for EmptyExecutor {
        async fn all_documents(&self, _ws: &str) -> Result<Vec<Document>> {
            Ok(vec![])
        }
        async fn soft_delete_document(&self, _ws: &str, _id: Uuid) -> Result<()> {
            Ok(())
        }
        async fn decay_workspace_edges(
            &self,
            _ws: &str,
            _lambda: f32,
            _now: DateTime<Utc>,
        ) -> Result<usize> {
            Ok(0)
        }
        async fn patrol_llm_complete(&self, _ws: &str, _prompt: &str) -> Result<String> {
            Ok(String::new())
        }
        async fn reingest_source_document(&self, _ws: &str, _id: Uuid) -> Result<bool> {
            Ok(false)
        }
    }

    async fn scheduler_fixture() -> (TempDir, Arc<PatrolScheduler>) {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path();
        let workspaces = Arc::new(WorkspaceManager::new(path).await.unwrap());
        workspaces.ensure_default().await.unwrap();
        let history = Arc::new(PatrolHistoryStore::new(path));
        let patrol = Arc::new(Patrol::new(
            Arc::new(EmptyExecutor),
            workspaces.clone(),
            Arc::new(SearchLogStore::new(path)),
            Arc::new(SyncStateStore::new(path)),
            Arc::new(ReviewQueueStore::new(path)),
            Arc::new(FeedbackStore::new(path)),
            Arc::new(FreshnessStore::new(path)),
            Arc::new(MetricsStore::new(path)),
            history.clone(),
        ));
        let scheduler = Arc::new(PatrolScheduler::new(patrol, workspaces, history));
        (tmp, scheduler)
    }

    #[tokio::test]
    async fn test_tick_runs_never_run_workspace() {
        let (_t, scheduler) = scheduler_fixture().await;
        // default는 personal 프리셋(frequency=weekly), 한 번도 안 돎 → due.
        let ran = scheduler.tick_once(Utc::now()).await;
        assert_eq!(ran, 1, "한 번도 안 돈 워크스페이스는 즉시 실행");
    }

    #[tokio::test]
    async fn test_tick_skips_recently_run() {
        let (_t, scheduler) = scheduler_fixture().await;
        // 1회 실행 → last_run 기록(weekly 주기).
        scheduler.tick_once(Utc::now()).await;
        // 곧바로 재틱 → 주기 미도래로 스킵.
        let ran = scheduler.tick_once(Utc::now()).await;
        assert_eq!(ran, 0, "주기 미도래면 재실행 안 함");
    }

    #[tokio::test]
    async fn test_tick_runs_again_after_interval() {
        let (_t, scheduler) = scheduler_fixture().await;
        scheduler.tick_once(Utc::now()).await;
        // weekly(604800s) 이후 시각으로 틱 → 다시 due.
        let later = Utc::now() + chrono::Duration::days(8);
        let ran = scheduler.tick_once(later).await;
        assert_eq!(ran, 1, "주기 경과 후 재실행");
    }
}
