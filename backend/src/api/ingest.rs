use axum::{
    extract::{Query, State},
    http::StatusCode,
    Extension, Json,
};
use std::sync::Arc;

use crate::api::{require_write, resolve_and_authorize_workspace, WorkspaceQuery};
use crate::auth::AuthContext;
use crate::models::api::{IngestRequest, IngestResponse};
use crate::AppState;

pub async fn ingest_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Query(wq): Query<WorkspaceQuery>,
    Json(req): Json<IngestRequest>,
) -> Result<Json<IngestResponse>, (StatusCode, String)> {
    require_write(&ctx)?;
    let workspace = resolve_and_authorize_workspace(&state, &ctx, wq.workspace).await?;

    state
        .indexer
        .ingest_to_workspace(req.content, &workspace)
        .await
        .map(Json)
        .map_err(|e| {
            tracing::error!("Ingest failed: {e:?}");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })
}
