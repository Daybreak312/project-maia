use axum::{
    extract::State,
    http::{header::SET_COOKIE, HeaderMap, StatusCode},
    response::{AppendHeaders, IntoResponse},
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::api::users::UserInfo;
use crate::auth::sessions::SESSION_TTL_DAYS;
use crate::auth::{
    extract_cookie, AuthContext, AuthSource, Permission, SessionManager, UserManager,
    WorkspaceScope, SESSION_COOKIE_NAME,
};
use crate::AppState;

/// 세션 쿠키 Max-Age(초) — 서버측 세션 TTL(30일)과 같은 시점을 가리킨다.
const SESSION_COOKIE_MAX_AGE_SECS: i64 = SESSION_TTL_DAYS * 24 * 60 * 60;

/// 로그인 요청
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

/// 로그인 응답 (세션 토큰은 body가 아니라 HttpOnly 쿠키로만 전달된다 —
/// XSS로 토큰을 읽어가는 경로를 차단)
#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub user: UserInfo,
}

/// `/api/auth/me` 응답의 워크스페이스 항목
#[derive(Debug, Serialize)]
pub struct WorkspaceAccess {
    pub id: String,
    pub permission: Permission,
}

/// `/api/auth/me` 응답
#[derive(Debug, Serialize)]
pub struct MeResponse {
    /// 인증 소스: "master" | "dev" | "api_key" | "session"
    pub auth_source: String,
    /// 인증된 계정 정보 (세션/소유 키 인증 시. 마스터키·시스템 키는 null)
    pub user: Option<UserInfo>,
    /// 전역(시스템) admin 여부
    pub is_admin: bool,
    /// 접근 가능한 워크스페이스와 유효 권한 목록
    pub workspaces: Vec<WorkspaceAccess>,
}

/// Set-Cookie 값을 구성한다. `max_age_secs` 0은 즉시 만료(로그아웃용).
///
/// HttpOnly(스크립트 접근 차단) + SameSite=Lax(교차 사이트 POST CSRF 완화)
/// + Path=/. Secure는 배포 환경(https) 기본 on, http 로컬 개발만 env로 off.
fn build_session_cookie(token: &str, secure: bool, max_age_secs: i64) -> String {
    let mut cookie = format!(
        "{}={}; HttpOnly; SameSite=Lax; Path=/; Max-Age={}",
        SESSION_COOKIE_NAME, token, max_age_secs
    );
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

/// 로그인 검증 + 세션 발급 (AppState 없이 테스트 가능한 코어).
/// 실패 사유(계정 없음/비밀번호 불일치)는 구분하지 않는다 — 열거 방지.
pub(crate) async fn perform_login(
    users: &UserManager,
    sessions: &SessionManager,
    username: &str,
    password: &str,
) -> Option<(crate::auth::User, String)> {
    let user = users.verify_password(username, password).await?;
    match sessions.create_session(&user.user_id).await {
        Ok((_, token)) => Some((user, token)),
        Err(e) => {
            tracing::error!("세션 발급 실패 (user={}): {}", user.user_id, e);
            None
        }
    }
}

/// 컨텍스트의 접근 가능 워크스페이스 목록을 산출한다 (순수 함수).
/// `All` 스코프는 존재하는 전체 워크스페이스, 그 외는 명시 스코프를
/// 존재하는 워크스페이스로 필터링한다 (삭제된 ws가 목록에 남지 않게).
pub(crate) fn accessible_workspaces(
    ctx: &AuthContext,
    existing_ws_ids: &[String],
) -> Vec<WorkspaceAccess> {
    match &ctx.scope {
        WorkspaceScope::All => existing_ws_ids
            .iter()
            .map(|id| WorkspaceAccess {
                id: id.clone(),
                permission: Permission::Admin,
            })
            .collect(),
        _ => ctx
            .scoped_workspaces()
            .into_iter()
            .filter(|(id, _)| existing_ws_ids.iter().any(|w| w == id))
            .map(|(id, permission)| WorkspaceAccess { id, permission })
            .collect(),
    }
}

/// POST /api/auth/login — ID/PW 로그인, 세션 쿠키 발급 (인증 불필요 라우트).
///
/// 실패는 username 존재 여부가 구분되지 않는 단일 401 메시지로 응답한다.
pub async fn login_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<LoginRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let Some((user, token)) =
        perform_login(&state.users, &state.sessions, &req.username, &req.password).await
    else {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Invalid username or password".to_string(),
        ));
    };

    let cookie = build_session_cookie(&token, state.cookie_secure, SESSION_COOKIE_MAX_AGE_SECS);
    tracing::info!("User logged in: {}", user.username);

    Ok((
        AppendHeaders([(SET_COOKIE, cookie)]),
        Json(LoginResponse {
            user: UserInfo::from(user),
        }),
    ))
}

/// POST /api/auth/logout — 세션 폐기 + 쿠키 제거 (인증 불필요 라우트).
///
/// 만료·무효 세션이어도 쿠키는 항상 지운다 (멱등 — 브라우저 상태 정리 우선).
pub async fn logout_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(token) = extract_cookie(&headers, SESSION_COOKIE_NAME) {
        if let Err(e) = state.sessions.revoke(&token).await {
            tracing::error!("로그아웃 세션 폐기 실패: {}", e);
        }
    }

    let clear = build_session_cookie("", state.cookie_secure, 0);
    (StatusCode::NO_CONTENT, AppendHeaders([(SET_COOKIE, clear)]))
}

/// GET /api/auth/me — 현재 인증 주체 정보 + 접근 가능 워크스페이스 목록.
pub async fn me_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
) -> Result<Json<MeResponse>, (StatusCode, String)> {
    let auth_source = match ctx.source {
        AuthSource::Master => "master",
        AuthSource::DevMode => "dev",
        AuthSource::ApiKey => "api_key",
        AuthSource::Session => "session",
    };

    // 계정 정보는 신원이 있을 때만 — 저장소에서 최신 상태를 다시 읽는다
    // (display_name·is_admin 변경이 me 응답에 즉시 반영되게).
    let user = match &ctx.user {
        Some(identity) => state.users.get(&identity.user_id).await.map(UserInfo::from),
        None => None,
    };

    let existing: Vec<String> = state
        .workspaces
        .list()
        .await
        .into_iter()
        .map(|w| w.id)
        .collect();

    Ok(Json(MeResponse {
        auth_source: auth_source.to_string(),
        user,
        is_admin: ctx.is_admin(),
        workspaces: accessible_workspaces(&ctx, &existing),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    // ──── build_session_cookie ────

    #[test]
    fn test_build_session_cookie_attributes() {
        let cookie = build_session_cookie("tok123", true, SESSION_COOKIE_MAX_AGE_SECS);
        assert!(cookie.starts_with("maia_session=tok123; "));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Lax"));
        assert!(cookie.contains("Path=/"));
        assert!(cookie.contains("Max-Age=2592000"), "30일 = 2592000초: {}", cookie);
        assert!(cookie.ends_with("; Secure"));
    }

    #[test]
    fn test_build_session_cookie_secure_toggle() {
        // http 로컬 개발용 — Secure를 끌 수 있어야 브라우저가 쿠키를 저장한다
        let cookie = build_session_cookie("tok", false, 100);
        assert!(!cookie.contains("Secure"));
    }

    #[test]
    fn test_build_session_cookie_logout_clears() {
        let cookie = build_session_cookie("", true, 0);
        assert!(cookie.starts_with("maia_session=; "));
        assert!(cookie.contains("Max-Age=0"), "Max-Age=0 = 즉시 만료(삭제)");
    }

    // ──── perform_login ────

    async fn setup() -> (TempDir, UserManager, SessionManager) {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_str().unwrap();
        let users = UserManager::new(dir).await.unwrap();
        let sessions = SessionManager::new(dir).await.unwrap();
        (tmp, users, sessions)
    }

    #[tokio::test]
    async fn test_perform_login_success_issues_session() {
        let (_tmp, users, sessions) = setup().await;
        users
            .create_user("alice", "password123", "A".to_string(), false)
            .await
            .unwrap();

        let (user, token) = perform_login(&users, &sessions, "Alice", "password123")
            .await
            .unwrap();
        assert_eq!(user.username, "alice");
        // 발급된 토큰으로 세션 인증이 되어야 한다
        assert_eq!(sessions.authenticate(&token).await.unwrap().user_id, user.user_id);
    }

    #[tokio::test]
    async fn test_perform_login_failures_indistinguishable() {
        let (_tmp, users, sessions) = setup().await;
        users
            .create_user("alice", "password123", "A".to_string(), false)
            .await
            .unwrap();

        // 잘못된 비밀번호 / 존재하지 않는 계정 — 둘 다 동일하게 None
        // (핸들러는 단일 401 메시지로 응답 → username 열거 방지)
        assert!(perform_login(&users, &sessions, "alice", "wrong").await.is_none());
        assert!(perform_login(&users, &sessions, "nobody", "password123").await.is_none());
    }

    // ──── accessible_workspaces ────

    fn existing(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn test_accessible_workspaces_all_scope_lists_everything() {
        let ctx = AuthContext::master();
        let list = accessible_workspaces(&ctx, &existing(&["default", "work"]));
        assert_eq!(list.len(), 2);
        assert!(list.iter().all(|w| w.permission == Permission::Admin));
    }

    #[test]
    fn test_accessible_workspaces_fixed_scope_filters_stale() {
        // 키 스코프에 삭제된 ws가 남아 있어도 목록에는 존재하는 것만 나온다
        let key = crate::auth::ApiKey {
            key_id: "maia_sk_x".to_string(),
            hashed_key: "sha256:x".to_string(),
            label: "t".to_string(),
            workspaces: vec!["alive".to_string(), "deleted".to_string()],
            permissions: Permission::ReadWrite,
            created_at: chrono::Utc::now(),
            last_used_at: None,
            expires_at: None,
            owner: None,
        };
        let ctx = AuthContext::from_api_key(&key);
        let list = accessible_workspaces(&ctx, &existing(&["alive", "other"]));
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "alive");
        assert_eq!(list[0].permission, Permission::ReadWrite);
    }

    #[test]
    fn test_accessible_workspaces_per_workspace_roles() {
        let user = crate::auth::User {
            user_id: "user_x".to_string(),
            username: "bob".to_string(),
            password_hash: "$argon2id$x".to_string(),
            display_name: "Bob".to_string(),
            is_admin: false,
            created_at: chrono::Utc::now(),
        };
        let mut access = BTreeMap::new();
        access.insert("mine".to_string(), Permission::Admin);
        access.insert("shared".to_string(), Permission::ReadOnly);
        let ctx = AuthContext::from_user(&user, access);

        let list = accessible_workspaces(&ctx, &existing(&["mine", "shared", "private"]));
        assert_eq!(list.len(), 2);
        assert!(list.iter().any(|w| w.id == "mine" && w.permission == Permission::Admin));
        assert!(list.iter().any(|w| w.id == "shared" && w.permission == Permission::ReadOnly));
    }
}
