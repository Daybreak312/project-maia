use axum::{
    extract::{Path, State},
    http::StatusCode,
    Extension, Json,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::api::require_admin;
use crate::auth::AuthContext;
use crate::workspace::{WorkspaceConfig, WorkspaceTemplate};
use crate::AppState;

/// 워크스페이스 생성 요청. template 미지정 시 personal.
#[derive(Debug, Deserialize)]
pub struct CreateWorkspaceRequest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub template: Option<WorkspaceTemplate>,
}

/// GET /api/workspaces — 전체 워크스페이스 목록 (admin)
pub async fn list_workspaces_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Json<Vec<WorkspaceConfig>>, (StatusCode, String)> {
    require_admin(&ctx)?;
    Ok(Json(state.workspaces.list().await))
}

/// GET /api/workspaces/:id — 단일 워크스페이스 조회 (admin)
pub async fn get_workspace_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<Json<WorkspaceConfig>, (StatusCode, String)> {
    require_admin(&ctx)?;
    state
        .workspaces
        .get(&id)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))
}

/// POST /api/workspaces — 워크스페이스 생성 (admin)
///
/// 생성과 동시에 Qdrant 컬렉션을 준비한다(best-effort). Qdrant 불가용 시에도
/// 워크스페이스 설정은 파일에 기록되며, 컬렉션은 최초 ingest 시 lazy 하게 보장된다.
pub async fn create_workspace_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Json(req): Json<CreateWorkspaceRequest>,
) -> Result<Json<WorkspaceConfig>, (StatusCode, String)> {
    require_admin(&ctx)?;

    let template = req.template.unwrap_or(WorkspaceTemplate::Personal);
    let config = WorkspaceConfig::new(req.id, req.name, template);

    let created = state
        .workspaces
        .create(config)
        .await
        // 유효성 실패·중복은 클라이언트 오류
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    if let Err(e) = state.indexer.provision_workspace_collection(&created.id).await {
        tracing::warn!(
            "Workspace '{}' created but Qdrant collection provisioning failed: {}",
            created.id,
            e
        );
    }

    Ok(Json(created))
}

/// DELETE /api/workspaces/:id — 워크스페이스 삭제 (admin)
///
/// raw 문서·설정을 제거하고 Qdrant 컬렉션도 정리한다(best-effort).
/// `default` 워크스페이스는 삭제할 수 없다.
pub async fn delete_workspace_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    require_admin(&ctx)?;

    state.workspaces.delete(&id).await.map_err(|e| {
        let msg = e.to_string();
        if msg.contains("Cannot delete") {
            (StatusCode::BAD_REQUEST, msg)
        } else if msg.contains("not found") {
            (StatusCode::NOT_FOUND, msg)
        } else {
            (StatusCode::INTERNAL_SERVER_ERROR, msg)
        }
    })?;

    if let Err(e) = state.indexer.purge_workspace_collection(&id).await {
        tracing::warn!(
            "Workspace '{}' deleted but Qdrant collection purge failed: {}",
            id,
            e
        );
    }

    Ok(StatusCode::NO_CONTENT)
}
