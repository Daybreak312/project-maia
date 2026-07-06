use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::models::api::{DocumentResponse, IngestResponse};
use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct UpdateRequest {
    pub content: String,
}

pub async fn get_document_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<DocumentResponse>, (StatusCode, String)> {
    state
        .indexer
        .get_document(id)
        .await
        .map(|doc| {
            Json(DocumentResponse {
                id: doc.id,
                raw_content: doc.raw_content,
                summary: doc.summary,
                entities: doc.entities,
                created_at: doc.created_at,
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
    Query(query): Query<RecentQuery>,
) -> Result<Json<ListResponse>, (StatusCode, String)> {
    state
        .indexer
        .recent(query.limit, query.offset)
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
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateRequest>,
) -> Result<Json<IngestResponse>, (StatusCode, String)> {
    state
        .indexer
        .update(id, payload.content)
        .await
        .map(Json)
        .map_err(|e| {
            tracing::error!("Update document failed: {e:?}");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })
}

pub async fn delete_document_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .indexer
        .delete(id)
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
) -> Result<Json<ReindexResponse>, (StatusCode, String)> {
    state
        .indexer
        .reindex_all()
        .await
        .map(|indexed| Json(ReindexResponse { indexed }))
        .map_err(|e| {
            tracing::error!("Reindex failed: {e:?}");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })
}
