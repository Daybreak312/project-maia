use axum::{
    extract::{Path, State},
    http::StatusCode,
    Extension, Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::api::require_admin;
use crate::auth::{ApiKey, AuthContext, AuthSource, Permission, UserIdentity};
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
    /// 소유 계정 user_id (None = 시스템 키)
    pub owner: Option<String>,
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
            owner: k.owner,
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
        .create_key(req.label, req.workspaces, req.permissions, req.expires_at, None)
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

// ──────────────────────────────────────────────────────────────
// /api/me/keys — 계정 셀프서비스 (로그인 세션 전용)
// ──────────────────────────────────────────────────────────────

/// 로그인 세션 인증을 요구하고 계정 신원을 돌려준다.
///
/// API 키 인증(소유 키 포함)은 거부한다 — 유출된 키 하나로 새 키를 계속
/// 발급하는 자기 증식 경로를 차단한다. 키 관리는 사람(세션)만 한다.
fn require_session_user(ctx: &AuthContext) -> Result<&UserIdentity, (StatusCode, String)> {
    if ctx.source != AuthSource::Session {
        return Err((
            StatusCode::FORBIDDEN,
            "Key self-service requires a login session".to_string(),
        ));
    }
    ctx.user.as_ref().ok_or((
        StatusCode::FORBIDDEN,
        "Key self-service requires a user identity".to_string(),
    ))
}

/// 셀프서비스 키 발급이 계정의 현재 접근권을 초과하지 않는지 검증 (순수 함수).
///
/// 런타임 교집합(from_owned_api_key)이 어차피 캡하지만, 발급 시점에 명시적
/// 400으로 거부해 "만들어졌는데 동작하지 않는 키"라는 놀람을 없앤다.
fn validate_self_key_scope(
    requested_workspaces: &[String],
    requested_permission: &Permission,
    user_access: &BTreeMap<String, Permission>,
) -> Result<(), (StatusCode, String)> {
    for ws in requested_workspaces {
        let Some(user_perm) = user_access.get(ws) else {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("You do not have access to workspace '{}'", ws),
            ));
        };
        // 요청 권한이 계정의 해당 ws 권한을 초과하면 거부
        // (요청 ∩ 보유 = 요청 이어야 통과)
        if &requested_permission.intersect(user_perm) != requested_permission {
            return Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "Requested permission exceeds your role in workspace '{}'",
                    ws
                ),
            ));
        }
    }
    Ok(())
}

/// GET /api/me/keys — 내 키 목록 (세션 전용)
pub async fn list_my_keys_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Json<Vec<ApiKeyInfo>>, (StatusCode, String)> {
    let identity = require_session_user(&ctx)?;
    let keys = state.api_keys.list_keys_for_owner(&identity.user_id).await;
    Ok(Json(keys.into_iter().map(ApiKeyInfo::from).collect()))
}

/// POST /api/me/keys — 내 키 발급 (세션 전용, admin 불필요).
/// 자기 접근권을 초과하는 스코프/권한은 400. 발급 키는 owner가 붙어
/// 이후에도 계정 접근권과 교집합으로만 동작한다.
pub async fn create_my_key_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Json(req): Json<CreateKeyRequest>,
) -> Result<Json<CreateKeyResponse>, (StatusCode, String)> {
    let identity = require_session_user(&ctx)?;
    validate_key_scope(&req.workspaces)?;

    // 계정의 "현재" 접근권 기준으로 검증한다 (글로벌 admin이면 전 ws Admin).
    let Some(user) = state.users.get(&identity.user_id).await else {
        return Err((StatusCode::UNAUTHORIZED, "User no longer exists".to_string()));
    };
    let user_access = state.auth.user_access_map(&user).await;
    validate_self_key_scope(&req.workspaces, &req.permissions, &user_access)?;

    let (key, raw) = state
        .api_keys
        .create_key(
            req.label,
            req.workspaces,
            req.permissions,
            req.expires_at,
            Some(identity.user_id.clone()),
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(CreateKeyResponse {
        api_key: raw,
        key: ApiKeyInfo::from(key),
    }))
}

/// DELETE /api/me/keys/:key_id — 내 키 폐기 (세션 전용).
/// 남의 키·시스템 키는 존재 여부를 노출하지 않고 404로 응답한다.
pub async fn revoke_my_key_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Path(key_id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let identity = require_session_user(&ctx)?;

    let owns = state
        .api_keys
        .list_keys_for_owner(&identity.user_id)
        .await
        .iter()
        .any(|k| k.key_id == key_id);
    if !owns {
        return Err((
            StatusCode::NOT_FOUND,
            format!("API key not found: {}", key_id),
        ));
    }

    state
        .api_keys
        .revoke_key(&key_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::keys::WorkspaceScope;

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

    // ──── require_session_user ────

    fn session_ctx() -> AuthContext {
        AuthContext {
            key_id: "user_x".to_string(),
            permissions: Permission::ReadOnly,
            scope: WorkspaceScope::PerWorkspace(BTreeMap::new()),
            is_master: false,
            source: AuthSource::Session,
            user: Some(UserIdentity {
                user_id: "user_x".to_string(),
                username: "x".to_string(),
            }),
        }
    }

    #[test]
    fn test_require_session_user_accepts_session_only() {
        assert!(require_session_user(&session_ctx()).is_ok());

        // 마스터키·API 키 인증은 셀프서비스 불가 (키의 자기 증식 차단)
        let (status, _) = require_session_user(&AuthContext::master()).unwrap_err();
        assert_eq!(status, StatusCode::FORBIDDEN);

        let mut key_ctx = session_ctx();
        key_ctx.source = AuthSource::ApiKey;
        assert!(
            require_session_user(&key_ctx).is_err(),
            "소유 키 인증이라도 키 관리는 세션 전용"
        );
    }

    // ──── validate_self_key_scope ────

    fn access(map: &[(&str, Permission)]) -> BTreeMap<String, Permission> {
        map.iter().map(|(w, p)| (w.to_string(), p.clone())).collect()
    }

    #[test]
    fn test_validate_self_key_scope_subset_ok() {
        let user_access = access(&[("a", Permission::ReadWrite), ("b", Permission::Admin)]);
        assert!(validate_self_key_scope(
            &["a".to_string()],
            &Permission::ReadOnly,
            &user_access
        )
        .is_ok());
        assert!(validate_self_key_scope(
            &["a".to_string(), "b".to_string()],
            &Permission::ReadWrite,
            &user_access
        )
        .is_ok());
    }

    #[test]
    fn test_validate_self_key_scope_workspace_outside_access_400() {
        let user_access = access(&[("a", Permission::ReadWrite)]);
        let (status, msg) = validate_self_key_scope(
            &["a".to_string(), "outside".to_string()],
            &Permission::ReadOnly,
            &user_access,
        )
        .unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(msg.contains("outside"));
    }

    #[test]
    fn test_validate_self_key_scope_permission_above_role_400() {
        // read_only 멤버가 read_write 키를 발급하려는 시도 → 400
        let user_access = access(&[("a", Permission::ReadOnly)]);
        let (status, _) = validate_self_key_scope(
            &["a".to_string()],
            &Permission::ReadWrite,
            &user_access,
        )
        .unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // admin 요청도 role이 read_write면 거부
        let user_access = access(&[("a", Permission::ReadWrite)]);
        assert!(validate_self_key_scope(&["a".to_string()], &Permission::Admin, &user_access)
            .is_err());
    }
}
