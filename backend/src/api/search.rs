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
        .search(req.query, req.limit, req.offset, req.mode)
        .await
        .map(Json)
        .map_err(|e| {
            tracing::error!("Search failed: {e:?}");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })
}
