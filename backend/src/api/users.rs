use axum::{
    extract::{Path, State},
    http::StatusCode,
    Extension, Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::api::require_admin;
use crate::auth::users::normalize_username;
use crate::auth::{AuthContext, AuthSource, User, UserManager};
use crate::workspace::{MembershipManager, WorkspaceManager};
use crate::AppState;

/// 계정의 공개 뷰 — password_hash를 노출하지 않는다.
#[derive(Debug, Serialize)]
pub struct UserInfo {
    pub user_id: String,
    pub username: String,
    pub display_name: String,
    pub is_admin: bool,
    pub created_at: DateTime<Utc>,
}

impl From<User> for UserInfo {
    fn from(u: User) -> Self {
        Self {
            user_id: u.user_id,
            username: u.username,
            display_name: u.display_name,
            is_admin: u.is_admin,
            created_at: u.created_at,
        }
    }
}

/// 계정 생성 요청 (admin 전용 — 셀프가입 없음). display_name 미지정 시 username.
#[derive(Debug, Deserialize)]
pub struct CreateUserRequest {
    pub username: String,
    /// 초기 비밀번호 (admin이 지정해 전달)
    pub password: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub is_admin: bool,
}

/// 계정 생성 응답. 자동 생성된 개인 워크스페이스 id를 함께 알려준다.
#[derive(Debug, Serialize)]
pub struct CreateUserResponse {
    pub user: UserInfo,
    pub personal_workspace: String,
}

/// 비밀번호 변경 요청
#[derive(Debug, Deserialize)]
pub struct ChangePasswordRequest {
    pub password: String,
}

/// 계정 생성 + 개인 워크스페이스(`u-{username}`) 자동 생성.
///
/// 반쪽 상태 금지 — 순서와 롤백:
/// 1. 개인 워크스페이스 id 충돌 선검사 (충돌 시 아무것도 만들지 않고 409)
/// 2. 계정 생성 (username 정규화·중복 검사는 UserManager가 수행)
/// 3. 워크스페이스 생성 — 실패 시 계정 롤백
/// 4. 본인 role=admin 멤버 등록 — 실패 시 워크스페이스+계정 롤백
///
/// AppState 없이 파일 기반 매니저만 받아 테스트 가능하다.
pub(crate) async fn create_user_with_personal_workspace(
    users: &UserManager,
    workspaces: &WorkspaceManager,
    memberships: &MembershipManager,
    req: CreateUserRequest,
) -> Result<(User, String), (StatusCode, String)> {
    let username = normalize_username(&req.username);
    let ws_id = format!("u-{}", username);

    // 1) 워크스페이스 id 선검사 — 계정을 만들기 전에 확정적으로 거른다.
    //    (삭제된 계정의 잔존 개인 ws 등과 충돌하면 admin이 먼저 정리해야 한다)
    if workspaces.exists(&ws_id).await {
        return Err((
            StatusCode::CONFLICT,
            format!(
                "Personal workspace '{}' already exists — resolve the conflict before creating this user",
                ws_id
            ),
        ));
    }

    // 2) 계정 생성
    let display_name = req.display_name.unwrap_or_else(|| username.clone());
    let user = users
        .create_user(&req.username, &req.password, display_name, req.is_admin)
        .await
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("already exists") {
                (StatusCode::CONFLICT, msg)
            } else {
                (StatusCode::BAD_REQUEST, msg)
            }
        })?;

    // 3) 개인 워크스페이스 생성 — 실패 시 계정 롤백
    let config = crate::workspace::WorkspaceConfig::new(
        ws_id.clone(),
        format!("{}의 개인 워크스페이스", user.display_name),
        crate::workspace::WorkspaceTemplate::Personal,
    );
    if let Err(e) = workspaces.create(config).await {
        rollback_user(users, &user.user_id).await;
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to create personal workspace: {}", e),
        ));
    }

    // 4) 본인을 role=admin 멤버로 등록 — 실패 시 워크스페이스+계정 롤백
    if let Err(e) = memberships
        .upsert_member(&ws_id, &user.user_id, crate::auth::Permission::Admin)
        .await
    {
        if let Err(we) = workspaces.delete(&ws_id).await {
            tracing::error!("개인 워크스페이스 롤백 실패 ({}): {}", ws_id, we);
        }
        memberships.forget_workspace(&ws_id).await;
        rollback_user(users, &user.user_id).await;
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to register personal workspace membership: {}", e),
        ));
    }

    Ok((user, ws_id))
}

/// 계정 생성 롤백 (best-effort — 실패는 로그로만 남긴다. 잔존 계정은
/// admin이 DELETE /api/users로 정리할 수 있다.)
async fn rollback_user(users: &UserManager, user_id: &str) {
    if let Err(e) = users.delete_user(user_id).await {
        tracing::error!("계정 생성 롤백 실패 ({}): {}", user_id, e);
    }
}

/// 계정 삭제 연쇄 정리. 반환값은 삭제된 계정.
///
/// - 소유 API 키 전부 폐기 (키가 계정보다 오래 살면 유령 접근 경로가 된다)
/// - 세션 전부 폐기
/// - 모든 워크스페이스 멤버십에서 제거 (유령 멤버 방지)
/// - 개인 워크스페이스는 삭제하지 **않는다** — 데이터(지식 원장)는 계정과
///   별개의 자산이므로 보존한다 (정보 유실 0 원칙). 필요 시 admin이
///   DELETE /api/workspaces/:id로 명시 삭제한다.
pub(crate) async fn delete_user_cascade(
    users: &UserManager,
    sessions: &crate::auth::SessionManager,
    api_keys: &crate::auth::ApiKeyManager,
    memberships: &MembershipManager,
    workspaces: &WorkspaceManager,
    user_id: &str,
) -> Result<User, (StatusCode, String)> {
    let removed = users.delete_user(user_id).await.map_err(|e| {
        let msg = e.to_string();
        if msg.contains("not found") {
            (StatusCode::NOT_FOUND, msg)
        } else {
            (StatusCode::INTERNAL_SERVER_ERROR, msg)
        }
    })?;

    if let Err(e) = api_keys.revoke_keys_for_owner(user_id).await {
        tracing::error!("계정 삭제 연쇄: 소유 키 폐기 실패 ({}): {}", user_id, e);
    }
    if let Err(e) = sessions.revoke_all_for_user(user_id).await {
        tracing::error!("계정 삭제 연쇄: 세션 폐기 실패 ({}): {}", user_id, e);
    }
    let ws_ids: Vec<String> = workspaces.list().await.into_iter().map(|w| w.id).collect();
    memberships.remove_user_everywhere(user_id, &ws_ids).await;

    Ok(removed)
}

/// POST /api/users — 계정 생성 (admin 전용, 초기 비밀번호 지정).
/// 개인 워크스페이스 `u-{username}`를 자동 생성하고 본인을 admin 멤버로 등록한다.
pub async fn create_user_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Json(req): Json<CreateUserRequest>,
) -> Result<Json<CreateUserResponse>, (StatusCode, String)> {
    require_admin(&ctx)?;

    let (user, ws_id) = create_user_with_personal_workspace(
        &state.users,
        &state.workspaces,
        &state.memberships,
        req,
    )
    .await?;

    // Qdrant 컬렉션 준비 (best-effort — 불가용 시 최초 ingest에서 lazy 보장)
    if let Err(e) = state.indexer.provision_workspace_collection(&ws_id).await {
        tracing::warn!(
            "Personal workspace '{}' created but Qdrant collection provisioning failed: {}",
            ws_id,
            e
        );
    }

    Ok(Json(CreateUserResponse {
        user: UserInfo::from(user),
        personal_workspace: ws_id,
    }))
}

/// GET /api/users — 계정 목록 (admin). 해시는 노출하지 않는다.
pub async fn list_users_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Json<Vec<UserInfo>>, (StatusCode, String)> {
    require_admin(&ctx)?;
    let users = state.users.list_users().await;
    Ok(Json(users.into_iter().map(UserInfo::from).collect()))
}

/// DELETE /api/users/:id — 계정 삭제 (admin).
/// 소유 키·세션·멤버십을 연쇄 정리한다. 본인 계정은 삭제할 수 없다
/// (실수로 유일 admin을 지워 잠기는 사고 방지 — 다른 admin이나 마스터키로 수행).
pub async fn delete_user_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Path(user_id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    require_admin(&ctx)?;

    if ctx.user.as_ref().is_some_and(|u| u.user_id == user_id) {
        return Err((
            StatusCode::BAD_REQUEST,
            "Cannot delete your own account".to_string(),
        ));
    }

    delete_user_cascade(
        &state.users,
        &state.sessions,
        &state.api_keys,
        &state.memberships,
        &state.workspaces,
        &user_id,
    )
    .await?;

    Ok(StatusCode::NO_CONTENT)
}

/// PUT /api/users/:id/password — 비밀번호 변경 (본인 세션 또는 admin).
///
/// API 키 인증으로는 본인 변경을 허용하지 않는다 — 유출된 키 하나로 계정
/// 자체(비밀번호)를 탈취하는 상승 경로를 차단한다. 변경 후 해당 계정의
/// 모든 세션을 폐기한다 (재로그인 강제 — 탈취 세션 무력화).
pub async fn change_password_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Path(user_id): Path<String>,
    Json(req): Json<ChangePasswordRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let is_self_session = ctx.source == AuthSource::Session
        && ctx.user.as_ref().is_some_and(|u| u.user_id == user_id);
    if !ctx.is_admin() && !is_self_session {
        return Err((
            StatusCode::FORBIDDEN,
            "Only the account owner (via session) or an admin can change this password".to_string(),
        ));
    }

    state
        .users
        .set_password(&user_id, &req.password)
        .await
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("not found") {
                (StatusCode::NOT_FOUND, msg)
            } else {
                (StatusCode::BAD_REQUEST, msg)
            }
        })?;

    if let Err(e) = state.sessions.revoke_all_for_user(&user_id).await {
        tracing::error!("비밀번호 변경 후 세션 폐기 실패 ({}): {}", user_id, e);
    }

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{ApiKeyManager, Permission, SessionManager};
    use tempfile::TempDir;

    struct Fixture {
        _tmp: TempDir,
        users: UserManager,
        sessions: SessionManager,
        api_keys: ApiKeyManager,
        memberships: MembershipManager,
        workspaces: WorkspaceManager,
    }

    async fn setup() -> Fixture {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_str().unwrap();
        let users = UserManager::new(dir).await.unwrap();
        let sessions = SessionManager::new(dir).await.unwrap();
        let api_keys = ApiKeyManager::new(dir).await.unwrap();
        let memberships = MembershipManager::new(tmp.path());
        let workspaces = WorkspaceManager::new(tmp.path()).await.unwrap();
        workspaces.ensure_default().await.unwrap();
        Fixture { _tmp: tmp, users, sessions, api_keys, memberships, workspaces }
    }

    fn req(username: &str) -> CreateUserRequest {
        CreateUserRequest {
            username: username.to_string(),
            password: "password123".to_string(),
            display_name: Some("Test User".to_string()),
            is_admin: false,
        }
    }

    #[tokio::test]
    async fn test_create_user_with_personal_workspace_happy_path() {
        let f = setup().await;
        let (user, ws_id) = create_user_with_personal_workspace(
            &f.users, &f.workspaces, &f.memberships, req("Alice"),
        )
        .await
        .unwrap();

        assert_eq!(ws_id, "u-alice", "정규화된 username 기반 개인 ws id");
        assert!(f.workspaces.exists("u-alice").await);
        // 본인이 role=admin 멤버로 등록되어야 한다
        let record = f.memberships.get("u-alice").await;
        assert_eq!(record.permission_of(&user.user_id), Some(Permission::Admin));
    }

    #[tokio::test]
    async fn test_create_user_workspace_conflict_rolls_back_nothing_created() {
        let f = setup().await;
        // 개인 ws id를 선점해 충돌 유발
        f.workspaces
            .create(crate::workspace::WorkspaceConfig::new(
                "u-alice".to_string(),
                "Squatter".to_string(),
                crate::workspace::WorkspaceTemplate::Personal,
            ))
            .await
            .unwrap();

        let err = create_user_with_personal_workspace(
            &f.users, &f.workspaces, &f.memberships, req("alice"),
        )
        .await
        .unwrap_err();

        assert_eq!(err.0, StatusCode::CONFLICT);
        // 반쪽 상태 금지: 계정이 만들어져 있으면 안 된다
        assert!(
            f.users.get_by_username("alice").await.is_none(),
            "ws 충돌 시 계정 생성 자체가 롤백(미생성)되어야 한다"
        );
    }

    #[tokio::test]
    async fn test_create_user_duplicate_username_no_workspace_leak() {
        let f = setup().await;
        create_user_with_personal_workspace(&f.users, &f.workspaces, &f.memberships, req("bob"))
            .await
            .unwrap();

        // 같은 username 재시도 — 계정 중복이지만 개인 ws는 이미 존재 → 409 선검사에 걸림
        let err = create_user_with_personal_workspace(
            &f.users, &f.workspaces, &f.memberships, req("BOB"),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);

        // 기존 계정·ws는 온전해야 한다
        assert!(f.users.get_by_username("bob").await.is_some());
        assert!(f.workspaces.exists("u-bob").await);
    }

    #[tokio::test]
    async fn test_create_user_invalid_username_rejected_before_side_effects() {
        let f = setup().await;
        let err = create_user_with_personal_workspace(
            &f.users, &f.workspaces, &f.memberships, req("bad name!"),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(!f.workspaces.exists("u-bad name!").await);
    }

    #[tokio::test]
    async fn test_delete_user_cascade_revokes_keys_sessions_memberships() {
        let f = setup().await;
        let (user, ws_id) = create_user_with_personal_workspace(
            &f.users, &f.workspaces, &f.memberships, req("carol"),
        )
        .await
        .unwrap();

        // 소유 키 + 세션 + 추가 멤버십 부여
        let (_, raw_key) = f
            .api_keys
            .create_key(
                "carol's".to_string(),
                vec![ws_id.clone()],
                Permission::ReadWrite,
                None,
                Some(user.user_id.clone()),
            )
            .await
            .unwrap();
        let (_, session_token) = f.sessions.create_session(&user.user_id).await.unwrap();
        f.memberships
            .upsert_member("default", &user.user_id, Permission::ReadOnly)
            .await
            .unwrap();

        let removed = delete_user_cascade(
            &f.users, &f.sessions, &f.api_keys, &f.memberships, &f.workspaces, &user.user_id,
        )
        .await
        .unwrap();
        assert_eq!(removed.user_id, user.user_id);

        // 연쇄 폐기 검증
        assert!(f.api_keys.authenticate(&raw_key).await.is_none(), "소유 키 폐기");
        assert!(f.sessions.authenticate(&session_token).await.is_none(), "세션 폐기");
        assert_eq!(f.memberships.get("default").await.permission_of(&user.user_id), None);
        assert_eq!(f.memberships.get(&ws_id).await.permission_of(&user.user_id), None);

        // 개인 워크스페이스(데이터)는 보존된다
        assert!(f.workspaces.exists(&ws_id).await, "개인 ws는 삭제하지 않는다 (데이터 보존)");
    }

    #[tokio::test]
    async fn test_delete_user_cascade_not_found() {
        let f = setup().await;
        let err = delete_user_cascade(
            &f.users, &f.sessions, &f.api_keys, &f.memberships, &f.workspaces, "user_ghost",
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }
}
