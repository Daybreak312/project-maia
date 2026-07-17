use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::api::{require_write, resolve_and_authorize_workspace, WorkspaceQuery};
use crate::auth::AuthContext;
use crate::models::api::{DocumentResponse, IngestResponse};
use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct UpdateRequest {
    pub content: String,
}

pub async fn get_document_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<Uuid>,
    Query(wq): Query<WorkspaceQuery>,
) -> Result<Json<DocumentResponse>, (StatusCode, String)> {
    let workspace = resolve_and_authorize_workspace(&state, &ctx, wq.workspace).await?;

    state
        .indexer
        .get_document_from_workspace(id, &workspace)
        .await
        .map(|doc| {
            Json(DocumentResponse {
                id: doc.id,
                raw_content: doc.raw_content,
                summary: doc.summary,
                entities: doc.entities,
                created_at: doc.created_at,
                source: doc.source,
            })
        })
        .map_err(|e| {
            tracing::error!("Get document failed: {e:?}");
            (StatusCode::NOT_FOUND, e.to_string())
        })
}

#[derive(Debug, Deserialize)]
pub struct RecentQuery {
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
    /// 대상 워크스페이스 (미지정 시 키 기본값)
    #[serde(default)]
    pub workspace: Option<String>,
}

fn default_limit() -> usize {
    20
}

#[derive(Debug, Serialize)]
pub struct ListResponse {
    pub documents: Vec<DocumentResponse>,
    pub total: usize,
    pub limit: usize,
    pub offset: usize,
}

pub async fn recent_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Query(query): Query<RecentQuery>,
) -> Result<Json<ListResponse>, (StatusCode, String)> {
    let workspace = resolve_and_authorize_workspace(&state, &ctx, query.workspace.clone()).await?;

    state
        .indexer
        .recent_in_workspace(query.limit, query.offset, &workspace)
        .await
        .map(|(docs, total)| {
            Json(ListResponse {
                documents: docs
                    .into_iter()
                    .map(|doc| DocumentResponse {
                        id: doc.id,
                        raw_content: doc.raw_content,
                        summary: doc.summary,
                        entities: doc.entities,
                        created_at: doc.created_at,
                        source: doc.source,
                    })
                    .collect(),
                total,
                limit: query.limit,
                offset: query.offset,
            })
        })
        .map_err(|e| {
            tracing::error!("Recent failed: {e:?}");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })
}

pub async fn update_document_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<Uuid>,
    Query(wq): Query<WorkspaceQuery>,
    Json(payload): Json<UpdateRequest>,
) -> Result<Json<IngestResponse>, (StatusCode, String)> {
    let workspace = resolve_and_authorize_workspace(&state, &ctx, wq.workspace).await?;
    require_write(&ctx, &workspace)?;

    state
        .indexer
        .update_in_workspace(id, payload.content, &workspace)
        .await
        .map(Json)
        .map_err(|e| {
            tracing::error!("Update document failed: {e:?}");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })
}

pub async fn delete_document_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<Uuid>,
    Query(wq): Query<WorkspaceQuery>,
) -> Result<StatusCode, (StatusCode, String)> {
    let workspace = resolve_and_authorize_workspace(&state, &ctx, wq.workspace).await?;
    require_write(&ctx, &workspace)?;

    state
        .indexer
        .delete_from_workspace(id, &workspace)
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(|e| {
            tracing::error!("Delete document failed: {e:?}");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })
}

#[derive(Serialize)]
pub struct ReindexResponse {
    pub indexed: usize,
}

pub async fn reindex_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Query(wq): Query<WorkspaceQuery>,
) -> Result<Json<ReindexResponse>, (StatusCode, String)> {
    let workspace = resolve_and_authorize_workspace(&state, &ctx, wq.workspace).await?;
    require_write(&ctx, &workspace)?;

    state
        .indexer
        .reindex_workspace(&workspace)
        .await
        .map(|indexed| Json(ReindexResponse { indexed }))
        .map_err(|e| {
            tracing::error!("Reindex failed: {e:?}");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })
}
