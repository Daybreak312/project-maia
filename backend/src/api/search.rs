use axum::{extract::State, http::StatusCode, Json};
use std::sync::Arc;

use crate::models::api::{SearchRequest, SearchResponse};
use crate::AppState;

pub async fn search_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SearchRequest>,
) -> Result<Json<SearchResponse>, (StatusCode, String)> {
    state
        .indexer
        .search(req.query, req.limit, req.offset, req.mode, req.tags)
        .await
        .map(Json)
        .map_err(|e| {
            tracing::error!("Search failed: {e:?}");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })
}

/// 모든 태그 조회
pub async fn tags_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<String>>, (StatusCode, String)> {
    state
        .indexer
        .get_all_tags()
        .await
        .map(Json)
        .map_err(|e| {
            tracing::error!("Get tags failed: {e:?}");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })
}
