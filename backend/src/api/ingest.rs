use axum::{extract::State, http::StatusCode, Json};
use std::sync::Arc;

use crate::models::api::{IngestRequest, IngestResponse};
use crate::AppState;

pub async fn ingest_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<IngestRequest>,
) -> Result<Json<IngestResponse>, (StatusCode, String)> {
    state
        .indexer
        .ingest(req.content)
        .await
        .map(Json)
        .map_err(|e| {
            tracing::error!("Ingest failed: {e:?}");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })
}
