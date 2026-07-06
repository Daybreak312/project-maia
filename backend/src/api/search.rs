use axum::{
    extract::{Query, State},
    http::StatusCode,
    Extension, Json,
};
use chrono::Utc;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::api::{resolve_and_authorize_workspace, WorkspaceQuery};
use crate::auth::AuthContext;
use crate::core::{
    cross_workspace_targets, DeepSearchParams, TimeSearchOptions, DEFAULT_DEEP_SEARCH_MAX_RESULTS,
};
use crate::models::api::{SearchRequest, SearchResponse};
use crate::storage::{derive_metrics, SearchLogRecord};
use crate::AppState;

pub async fn search_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Query(wq): Query<WorkspaceQuery>,
    Json(req): Json<SearchRequest>,
) -> Result<Json<SearchResponse>, (StatusCode, String)> {
    // 검색은 읽기 작업 — 인증만 되면 read_only 키도 허용된다.
    let primary = resolve_and_authorize_workspace(&state, &ctx, wq.workspace).await?;

    // primary 워크스페이스 설정을 한 번 읽어 교차 검색 목록·감쇠 강도(lambda)와
    // agent 검색 파라미터(확장 깊이·시간 상한)를 얻는다.
    let primary_config = state.workspaces.get(&primary).await.ok();
    let lambda = primary_config
        .as_ref()
        .map(|c| c.search.time_decay_lambda)
        .unwrap_or(0.0);
    let expansion_depth = primary_config
        .as_ref()
        .map(|c| c.search.graph_expansion_depth)
        .unwrap_or(1);
    let time_limit_ms = primary_config
        .as_ref()
        .map(|c| c.search.deep_search_time_limit_ms)
        .unwrap_or(15_000);

    // 교차 검색 대상 워크스페이스 집합 결정.
    // cross_workspace == Some(false)이면 단일 검색으로 강제, 그 외에는 워크스페이스
    // 설정의 cross_workspace 목록을 접근 권한·존재 여부로 필터링해 사용한다.
    let targets = if req.cross_workspace == Some(false) {
        vec![primary.clone()]
    } else {
        let cross_list = primary_config
            .map(|cfg| cfg.search.cross_workspace)
            .unwrap_or_default();

        let existing: HashSet<String> = state
            .workspaces
            .list()
            .await
            .into_iter()
            .map(|w| w.id)
            .collect();

        cross_workspace_targets(&primary, &cross_list, &ctx, &existing)
    };

    // 시간 인식 옵션: 감쇠는 요청에서 opt-in, 강도는 워크스페이스 설정을 따른다.
    let time = TimeSearchOptions {
        decay: req.time_decay.unwrap_or(false),
        lambda,
        since: req.since,
        until: req.until,
    };

    // agent 모드는 opt-in. 미지정/false면 기존 단일 검색 동작(기본 동작 불변).
    let agent_mode = req.agent == Some(true);
    let query_for_log = req.query.clone();
    let start = Instant::now();

    let result = if agent_mode {
        let params = DeepSearchParams {
            expansion_depth,
            time_limit: Duration::from_millis(time_limit_ms),
            max_results: DEFAULT_DEEP_SEARCH_MAX_RESULTS,
        };
        state
            .indexer
            .deep_search_across_workspaces(req.query, &targets, params)
            .await
    } else {
        state
            .indexer
            .search_across_workspaces(req.query, req.limit, req.offset, req.mode, &targets, time)
            .await
    };
    let duration_ms = start.elapsed().as_millis() as u64;

    // 검색 로그를 best-effort로 축적한다 — 로그 실패가 검색을 실패시키지 않는다.
    // (Phase 5 거버넌스 신호: zero-result 추이·품질 관측용.)
    if let Ok(resp) = &result {
        let (count, top, zero) = derive_metrics(&resp.results);
        let record = SearchLogRecord {
            timestamp: Utc::now(),
            workspace: primary.clone(),
            query: query_for_log,
            mode: resp.mode.clone(),
            result_count: count,
            top_score: top,
            zero_result: zero,
            duration_ms,
            rounds: resp.agent.as_ref().map(|a| a.rounds),
        };
        state.search_logs.append_best_effort(record).await;
    }

    result.map(Json).map_err(|e| {
        tracing::error!("Search failed: {e:?}");
        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })
}
