use axum::{
    extract::{Query, State},
    http::StatusCode,
    Extension, Json,
};
use std::collections::HashSet;
use std::sync::Arc;

use crate::api::{resolve_and_authorize_workspace, WorkspaceQuery};
use crate::auth::AuthContext;
use crate::core::cross_workspace_targets;
use crate::models::api::{SearchRequest, SearchResponse};
use crate::AppState;

pub async fn search_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Query(wq): Query<WorkspaceQuery>,
    Json(req): Json<SearchRequest>,
) -> Result<Json<SearchResponse>, (StatusCode, String)> {
    // 검색은 읽기 작업 — 인증만 되면 read_only 키도 허용된다.
    let primary = resolve_and_authorize_workspace(&state, &ctx, wq.workspace).await?;

    // 교차 검색 대상 워크스페이스 집합 결정.
    // cross_workspace == Some(false)이면 단일 검색으로 강제, 그 외에는 워크스페이스
    // 설정의 cross_workspace 목록을 접근 권한·존재 여부로 필터링해 사용한다.
    let targets = if req.cross_workspace == Some(false) {
        vec![primary.clone()]
    } else {
        let cross_list = state
            .workspaces
            .get(&primary)
            .await
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

    state
        .indexer
        .search_across_workspaces(req.query, req.limit, req.offset, req.mode, &targets)
        .await
        .map(Json)
        .map_err(|e| {
            tracing::error!("Search failed: {e:?}");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })
}
