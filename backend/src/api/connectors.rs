//! 커넥터 관리 API — 목록/등록/삭제/즉시 실행/상태.
//!
//! 권한: 목록·상태는 워크스페이스 접근(read)이면 되고, 등록·삭제·즉시 실행은 admin이다
//! (파일시스템 경로 등록·LLM 비용을 수반하므로 보수적으로 admin으로 잠근다).
//!
//! 즉시 실행은 백그라운드 태스크로 spawn해 요청 경로와 격리한다(동기화 중에도 API 응답성
//! 유지). 진행 상황은 상태 API로 관측한다.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::api::{require_admin, resolve_and_authorize_workspace, WorkspaceQuery};
use crate::auth::AuthContext;
use crate::connectors::runner::{RunProgress, SyncOptions};
use crate::connectors::sync_state::SyncState;
use crate::connectors::ConnectorIngestMode;
use crate::workspace::ConnectorInstance;
use crate::AppState;

/// 커넥터 한 개의 조회 뷰 — 설정 + 동기화 상태 + (실행 중이면) 실시간 진행.
#[derive(Debug, Serialize)]
pub struct ConnectorView {
    pub instance: ConnectorInstance,
    pub state: SyncState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<RunProgress>,
}

/// GET /api/connectors?workspace= — 워크스페이스의 커넥터 목록 + 상태.
pub async fn list_connectors_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Query(wq): Query<WorkspaceQuery>,
) -> Result<Json<Vec<ConnectorView>>, (StatusCode, String)> {
    let workspace = resolve_and_authorize_workspace(&state, &ctx, wq.workspace).await?;
    let config = state
        .workspaces
        .get(&workspace)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;

    let mut views = Vec::with_capacity(config.connectors.len());
    for instance in config.connectors {
        let sync_state = state
            .sync_state
            .load(&workspace, &instance.id)
            .await
            .unwrap_or_default();
        let progress = state.connector_runner.progress(&workspace, &instance.id).await;
        views.push(ConnectorView {
            instance,
            state: sync_state,
            progress,
        });
    }
    Ok(Json(views))
}

/// GET /api/connectors/:id/status?workspace= — 단일 커넥터 상태.
pub async fn connector_status_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    Query(wq): Query<WorkspaceQuery>,
) -> Result<Json<ConnectorView>, (StatusCode, String)> {
    let workspace = resolve_and_authorize_workspace(&state, &ctx, wq.workspace).await?;
    let config = state
        .workspaces
        .get(&workspace)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;

    let instance = config
        .connectors
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Connector '{id}' not found")))?;

    let sync_state = state.sync_state.load(&workspace, &id).await.unwrap_or_default();
    let progress = state.connector_runner.progress(&workspace, &id).await;
    Ok(Json(ConnectorView {
        instance,
        state: sync_state,
        progress,
    }))
}

/// POST /api/connectors?workspace= — 커넥터 등록 (admin).
pub async fn register_connector_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Query(wq): Query<WorkspaceQuery>,
    Json(instance): Json<ConnectorInstance>,
) -> Result<(StatusCode, Json<ConnectorInstance>), (StatusCode, String)> {
    require_admin(&ctx)?;
    let workspace = resolve_and_authorize_workspace(&state, &ctx, wq.workspace).await?;

    // 유효성 검사 — 잘못된 설정은 클라이언트 오류.
    instance
        .validate()
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    let mut config = state
        .workspaces
        .get(&workspace)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;

    if config.connectors.iter().any(|c| c.id == instance.id) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Connector '{}' already exists in workspace '{}'", instance.id, workspace),
        ));
    }

    config.connectors.push(instance.clone());
    state
        .workspaces
        .update(&workspace, config)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok((StatusCode::CREATED, Json(instance)))
}

/// DELETE /api/connectors/:id?workspace= — 커넥터 삭제 (admin).
///
/// 설정에서 제거하고 동기화 상태 파일도 정리한다. 유입된 문서는 삭제하지 않는다
/// (지식은 보존 — 소스 연결만 끊는다).
pub async fn delete_connector_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    Query(wq): Query<WorkspaceQuery>,
) -> Result<StatusCode, (StatusCode, String)> {
    require_admin(&ctx)?;
    let workspace = resolve_and_authorize_workspace(&state, &ctx, wq.workspace).await?;

    let mut config = state
        .workspaces
        .get(&workspace)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;

    let before = config.connectors.len();
    config.connectors.retain(|c| c.id != id);
    if config.connectors.len() == before {
        return Err((StatusCode::NOT_FOUND, format!("Connector '{id}' not found")));
    }

    state
        .workspaces
        .update(&workspace, config)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // 동기화 상태 파일 정리 (best-effort — 실패해도 삭제는 성공으로 본다).
    if let Err(e) = state.sync_state.delete(&workspace, &id).await {
        tracing::warn!("커넥터 '{id}' 상태 파일 삭제 실패(무해): {e}");
    }

    Ok(StatusCode::NO_CONTENT)
}

/// 즉시 실행 요청 바디.
#[derive(Debug, Deserialize, Default)]
pub struct SyncTriggerRequest {
    /// 유입 모드: "parsed"(기본) | "raw".
    #[serde(default)]
    pub mode: Option<String>,
    /// true면 커서를 무시하고 전체를 재스캔한다(대량 재적재).
    #[serde(default)]
    pub full: bool,
    /// 동시성 오버라이드.
    #[serde(default)]
    pub concurrency: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct SyncTriggerResponse {
    pub status: &'static str,
    pub workspace: String,
    pub connector_id: String,
}

/// POST /api/connectors/:id/sync?workspace= — 즉시 실행 트리거 (admin).
///
/// 백그라운드 태스크로 spawn하고 즉시 202를 반환한다. 진행 상황은 상태 API로 관측한다.
pub async fn trigger_connector_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
    Query(wq): Query<WorkspaceQuery>,
    Json(req): Json<SyncTriggerRequest>,
) -> Result<(StatusCode, Json<SyncTriggerResponse>), (StatusCode, String)> {
    require_admin(&ctx)?;
    let workspace = resolve_and_authorize_workspace(&state, &ctx, wq.workspace).await?;

    // 존재하지 않는 커넥터는 spawn 전에 404로 거른다(스폰된 태스크의 에러는 응답에 못 실림).
    let config = state
        .workspaces
        .get(&workspace)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
    if !config.connectors.iter().any(|c| c.id == id) {
        return Err((StatusCode::NOT_FOUND, format!("Connector '{id}' not found")));
    }

    // 이미 실행 중이면 409로 즉답한다(이중 클릭 방어). 데이터 무결성의 보증은 run_sync 내부의
    // 원자적 claim이며, 이 검사는 그 앞단의 빠른 피드백이다(미세 경합 시엔 내부 claim이 최종
    // 판정 — 중복 문서는 어느 경로에서도 생기지 않는다).
    if state.connector_runner.is_running(&workspace, &id) {
        return Err((
            StatusCode::CONFLICT,
            format!("Connector '{id}' is already running"),
        ));
    }

    let opts = SyncOptions {
        mode: ConnectorIngestMode::from_str_or_default(req.mode.as_deref()),
        full: req.full,
        concurrency: req.concurrency,
    };

    // 백그라운드 실행 — 요청 경로와 격리(동기화 중에도 API 응답성 유지).
    let runner = state.connector_runner.clone();
    let ws = workspace.clone();
    let cid = id.clone();
    tokio::spawn(async move {
        match runner.run_sync(&ws, &cid, opts).await {
            Ok(summary) => tracing::info!(
                "수동 트리거 동기화 완료 '{cid}'(ws={ws}): 신규 {}, 갱신 {}, 실패 {}",
                summary.created,
                summary.updated,
                summary.failed
            ),
            Err(e) => tracing::warn!("수동 트리거 동기화 실패 '{cid}'(ws={ws}): {e}"),
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(SyncTriggerResponse {
            status: "started",
            workspace,
            connector_id: id,
        }),
    ))
}
