use axum::{
    extract::{Path, State},
    http::StatusCode,
    Extension, Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::api::require_admin;
use crate::auth::{ApiKey, AuthContext, Permission};
use crate::AppState;

/// API 키의 공개 뷰 — 해시(hashed_key)를 노출하지 않는다.
#[derive(Debug, Serialize)]
pub struct ApiKeyInfo {
    pub key_id: String,
    pub label: String,
    pub workspaces: Vec<String>,
    pub permissions: Permission,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
}

impl From<ApiKey> for ApiKeyInfo {
    fn from(k: ApiKey) -> Self {
        Self {
            key_id: k.key_id,
            label: k.label,
            workspaces: k.workspaces,
            permissions: k.permissions,
            created_at: k.created_at,
            last_used_at: k.last_used_at,
            expires_at: k.expires_at,
        }
    }
}

/// 키 발급 요청. permissions 미지정 시 read_write.
#[derive(Debug, Deserialize)]
pub struct CreateKeyRequest {
    pub label: String,
    #[serde(default)]
    pub workspaces: Vec<String>,
    #[serde(default = "default_permission")]
    pub permissions: Permission,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

fn default_permission() -> Permission {
    Permission::ReadWrite
}

/// 키 발급 응답. 평문 키(`api_key`)는 이 응답에서만 확인 가능하다.
#[derive(Debug, Serialize)]
pub struct CreateKeyResponse {
    /// 평문 API 키 — 저장소에는 해시만 남으며 다시 조회할 수 없다.
    pub api_key: String,
    pub key: ApiKeyInfo,
}

/// 발급 요청의 워크스페이스 스코프를 검증한다 (fail-closed).
///
/// 영속 키는 반드시 1개 이상의 워크스페이스를 명시해야 한다. 빈 스코프를 허용하면
/// (과거) 전 워크스페이스 접근을 조용히 부여했고, `can_access_workspace` fail-closed
/// 이후에는 접근 불가한 무용 키가 되므로 어느 쪽이든 400으로 거부한다.
/// "unscoped = all"은 마스터키(MAIA_API_KEY, env) 전용 의미다.
fn validate_key_scope(workspaces: &[String]) -> Result<(), (StatusCode, String)> {
    if workspaces.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "At least one workspace must be specified (unscoped keys are not allowed)".to_string(),
        ));
    }
    Ok(())
}

/// GET /api/keys — 키 목록 (admin). 해시는 노출하지 않는다.
pub async fn list_keys_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Json<Vec<ApiKeyInfo>>, (StatusCode, String)> {
    require_admin(&ctx)?;
    let keys = state.api_keys.list_keys().await;
    Ok(Json(keys.into_iter().map(ApiKeyInfo::from).collect()))
}

/// POST /api/keys — 키 발급 (admin). 평문 키는 응답에서 1회만 노출된다.
pub async fn create_key_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Json(req): Json<CreateKeyRequest>,
) -> Result<Json<CreateKeyResponse>, (StatusCode, String)> {
    require_admin(&ctx)?;
    validate_key_scope(&req.workspaces)?;

    let (key, raw) = state
        .api_keys
        .create_key(req.label, req.workspaces, req.permissions, req.expires_at)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(CreateKeyResponse {
        api_key: raw,
        key: ApiKeyInfo::from(key),
    }))
}

/// DELETE /api/keys/:key_id — 키 폐기 (admin)
pub async fn revoke_key_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Path(key_id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    require_admin(&ctx)?;

    state.api_keys.revoke_key(&key_id).await.map_err(|e| {
        let msg = e.to_string();
        if msg.contains("not found") {
            (StatusCode::NOT_FOUND, msg)
        } else {
            (StatusCode::INTERNAL_SERVER_ERROR, msg)
        }
    })?;

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_key_scope_rejects_empty() {
        // 빈 스코프는 400으로 거부되어야 한다 (fail-open 격리 우회 방지).
        let (status, _) = validate_key_scope(&[]).unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_validate_key_scope_accepts_non_empty() {
        assert!(validate_key_scope(&["work".to_string()]).is_ok());
        assert!(validate_key_scope(&["a".to_string(), "b".to_string()]).is_ok());
    }
}
