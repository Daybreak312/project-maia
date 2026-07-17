pub mod keys;
pub mod sessions;
pub mod users;

pub use keys::{
    ApiKey, ApiKeyManager, AuthContext, AuthSource, Permission, UserIdentity, WorkspaceScope,
    generate_key_id, generate_raw_key, hash_key,
};
pub use sessions::{SessionManager, SESSION_COOKIE_NAME};
pub use users::{User, UserManager};

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Request, StatusCode},
    middleware::Next,
    response::Response,
};

use crate::workspace::{MembershipManager, WorkspaceManager};
use crate::AppState;

/// Authorization 헤더에서 Bearer 토큰을 추출한다.
fn extract_bearer(req: &Request<Body>) -> Option<String> {
    req.headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(|s| s.to_string())
}

/// Cookie 헤더에서 특정 이름의 쿠키 값을 추출한다.
/// 헤더가 여러 개이거나 한 헤더에 세미콜론으로 여러 쿠키가 있어도 처리한다.
pub fn extract_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    for header in headers.get_all(axum::http::header::COOKIE) {
        let Ok(raw) = header.to_str() else { continue };
        for pair in raw.split(';') {
            if let Some((k, v)) = pair.trim().split_once('=') {
                if k.trim() == name {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
}

/// 타이밍 공격을 방지하기 위한 상수 시간 바이트 비교.
/// 길이가 다르면 즉시 false (길이는 비밀이 아니므로 노출 허용).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ──────────────────────────────────────────────────────────────
// AuthEngine — 자격증명 해석기 (미들웨어의 순수 판정 코어)
// ──────────────────────────────────────────────────────────────

/// 요청 자격증명(Bearer 토큰 / 세션 쿠키)을 AuthContext로 해석하는 엔진.
///
/// HTTP 레이어(미들웨어)에서 분리되어 있어, Qdrant 등 무거운 인프라 없이
/// 파일 기반 매니저들만으로 인증 해석 순서 전체를 테스트할 수 있다.
///
/// 해석 순서: 개발모드(명시적 옵트인) → Bearer 마스터키 → Bearer 등록 API 키
/// → 세션 쿠키 → 실패(401). 유효하지 않은 자격증명은 다음 순서로 넘어간다
/// (첫 "일치"가 승리) — 어느 경로든 최종 산출물은 동일한 AuthContext이므로
/// 하위 핸들러는 인증 소스를 모른다.
pub struct AuthEngine {
    /// 마스터키(MAIA_API_KEY). 이제 선택적 부트스트랩 수단 — 없어도
    /// users/api_keys/세션으로 정상 인증한다 (과거의 fail-open 아님).
    master_key: Option<String>,
    /// MAIA_DEV_NO_AUTH=1 명시적 개발 모드 — 이때만 인증을 건너뛴다.
    dev_no_auth: bool,
    users: Arc<UserManager>,
    sessions: Arc<SessionManager>,
    api_keys: Arc<ApiKeyManager>,
    memberships: Arc<MembershipManager>,
    workspaces: Arc<WorkspaceManager>,
}

impl AuthEngine {
    pub fn new(
        master_key: Option<String>,
        dev_no_auth: bool,
        users: Arc<UserManager>,
        sessions: Arc<SessionManager>,
        api_keys: Arc<ApiKeyManager>,
        memberships: Arc<MembershipManager>,
        workspaces: Arc<WorkspaceManager>,
    ) -> Self {
        Self {
            master_key,
            dev_no_auth,
            users,
            sessions,
            api_keys,
            memberships,
            workspaces,
        }
    }

    /// 자격증명을 해석해 AuthContext를 산출한다. 모두 실패하면 None (→ 401).
    pub async fn authenticate(
        &self,
        bearer: Option<&str>,
        session_token: Option<&str>,
    ) -> Option<AuthContext> {
        // 0) 명시적 개발 모드 (MAIA_DEV_NO_AUTH=1) — 이때만 인증 skip.
        //    과거 "마스터키 미설정 = 전체 개방"(fail-open)은 제거되었다.
        if self.dev_no_auth {
            return Some(AuthContext::dev_mode());
        }

        if let Some(token) = bearer {
            // 1) 마스터키 (상수 시간 비교)
            if let Some(master) = &self.master_key {
                if constant_time_eq(token.as_bytes(), master.as_bytes()) {
                    return Some(AuthContext::master());
                }
            }

            // 2) 등록된 API 키 (해시 조회 + 만료 체크)
            if let Some(key) = self.api_keys.authenticate(token).await {
                match self.context_for_key(&key).await {
                    Some(ctx) => return Some(ctx),
                    None => {
                        // 소유 계정이 사라진 키 — 사실상 폐기 상태 (fail-closed).
                        // 무효 자격증명으로 취급하고 다음 순서로 넘어간다.
                        tracing::warn!(
                            "Rejected API key {}: owner account missing",
                            key.key_id
                        );
                    }
                }
            }
        }

        // 3) 세션 쿠키
        if let Some(token) = session_token {
            if let Some(session) = self.sessions.authenticate(token).await {
                match self.users.get(&session.user_id).await {
                    Some(user) => return Some(self.context_for_user(&user).await),
                    None => {
                        // 계정이 삭제된 세션 — fail-closed
                        tracing::warn!(
                            "Rejected session: user {} missing",
                            session.user_id
                        );
                    }
                }
            }
        }

        // 4) 어느 것과도 불일치
        None
    }

    /// API 키를 AuthContext로 변환한다.
    /// 소유 계정이 있는 키는 계정 접근권과 교집합하며, 계정이 없으면 None(무효).
    async fn context_for_key(&self, key: &ApiKey) -> Option<AuthContext> {
        match &key.owner {
            None => Some(AuthContext::from_api_key(key)),
            Some(owner_id) => {
                let user = self.users.get(owner_id).await?;
                let access = self.user_access_map(&user).await;
                Some(AuthContext::from_owned_api_key(key, &user, &access))
            }
        }
    }

    /// 계정을 AuthContext로 변환한다 (세션 인증 경로).
    pub async fn context_for_user(&self, user: &User) -> AuthContext {
        if user.is_admin {
            // 글로벌 admin은 scope=All — 접근 맵 계산이 불필요하다.
            return AuthContext::from_user(user, BTreeMap::new());
        }
        let access = self.user_access_map(user).await;
        AuthContext::from_user(user, access)
    }

    /// 계정의 현재 접근권 맵 (워크스페이스 ID → 유효 권한).
    /// 글로벌 admin은 존재하는 전 워크스페이스 Admin, 일반 계정은
    /// 멤버인 ws ∪ public ws.
    pub async fn user_access_map(&self, user: &User) -> BTreeMap<String, Permission> {
        let ws_ids: Vec<String> = self
            .workspaces
            .list()
            .await
            .into_iter()
            .map(|w| w.id)
            .collect();

        if user.is_admin {
            ws_ids
                .into_iter()
                .map(|id| (id, Permission::Admin))
                .collect()
        } else {
            self.memberships
                .access_map_for_user(&user.user_id, &ws_ids)
                .await
        }
    }

    /// 인증 수단이 하나도 없는 상태인지 검사하고 복구 방법을 안내한다 (부팅 시).
    /// 잠금은 유지된다 — 전 요청 401이 되지만 열어주지는 않는다 (fail-closed).
    pub async fn warn_if_locked_out(&self) {
        if self.dev_no_auth || self.master_key.is_some() {
            return;
        }
        if !self.users.has_users().await && !self.api_keys.has_keys().await {
            tracing::error!(
                "인증 수단이 하나도 없습니다 (계정 0, API 키 0, 마스터키 미설정). \
                 모든 요청이 401로 거부됩니다. 복구: MAIA_API_KEY 환경변수로 마스터키를 \
                 설정해 재기동한 뒤 계정(POST /api/users)이나 키를 만드세요. \
                 (로컬 개발용 인증 해제는 MAIA_DEV_NO_AUTH=1 — 프로덕션 금지)"
            );
        }
    }
}

// ──────────────────────────────────────────────────────────────
// 미들웨어
// ──────────────────────────────────────────────────────────────

/// 통합 인증 미들웨어.
///
/// 해석 순서: Bearer 마스터키 → Bearer 등록 API 키 → 세션 쿠키 → 401
/// (판정 자체는 `AuthEngine::authenticate`). 성공 시 `AuthContext`를
/// request extension에 주입해 하위 핸들러가 인증 소스와 무관하게
/// 워크스페이스 접근·권한을 판단하게 한다.
///
/// 등록된 API 키로 인증되면 `last_used_at`을 비블로킹(tokio::spawn)으로
/// 갱신해 응답 지연을 피한다.
pub async fn require_auth(
    State(state): State<Arc<AppState>>,
    mut req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let bearer = extract_bearer(&req);
    let session_token = extract_cookie(req.headers(), SESSION_COOKIE_NAME);

    let Some(ctx) = state
        .auth
        .authenticate(bearer.as_deref(), session_token.as_deref())
        .await
    else {
        return Err(StatusCode::UNAUTHORIZED);
    };

    // 등록 키 인증 시 last_used_at 비블로킹 갱신 (요청 경로 블로킹 금지).
    // 마스터/dev/세션은 key_id가 실제 키가 아니므로 제외한다.
    if ctx.source == AuthSource::ApiKey {
        let manager = state.api_keys.clone();
        let key_id = ctx.key_id.clone();
        tokio::spawn(async move {
            if let Err(e) = manager.update_last_used(&key_id).await {
                tracing::warn!("Failed to update last_used_at for {}: {}", key_id, e);
            }
        });
    }

    req.extensions_mut().insert(ctx);
    Ok(next.run(req).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{WorkspaceConfig, WorkspaceTemplate, WorkspaceVisibility};
    use tempfile::TempDir;

    #[test]
    fn test_constant_time_eq_equal() {
        assert!(constant_time_eq(b"secret-key", b"secret-key"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn test_constant_time_eq_different() {
        assert!(!constant_time_eq(b"secret-key", b"secret-kez"));
        assert!(!constant_time_eq(b"short", b"longer-key"));
        assert!(!constant_time_eq(b"a", b""));
    }

    // ──── extract_cookie ────

    fn headers_with_cookie(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.append(
            axum::http::header::COOKIE,
            axum::http::HeaderValue::from_str(value).unwrap(),
        );
        headers
    }

    #[test]
    fn test_extract_cookie_single() {
        let headers = headers_with_cookie("maia_session=abc123");
        assert_eq!(extract_cookie(&headers, "maia_session"), Some("abc123".to_string()));
    }

    #[test]
    fn test_extract_cookie_among_multiple() {
        let headers = headers_with_cookie("theme=dark; maia_session=tok42; lang=ko");
        assert_eq!(extract_cookie(&headers, "maia_session"), Some("tok42".to_string()));
    }

    #[test]
    fn test_extract_cookie_missing() {
        let headers = headers_with_cookie("theme=dark; lang=ko");
        assert_eq!(extract_cookie(&headers, "maia_session"), None);
        assert_eq!(extract_cookie(&HeaderMap::new(), "maia_session"), None);
    }

    #[test]
    fn test_extract_cookie_multiple_headers() {
        let mut headers = headers_with_cookie("theme=dark");
        headers.append(
            axum::http::header::COOKIE,
            axum::http::HeaderValue::from_static("maia_session=second"),
        );
        assert_eq!(extract_cookie(&headers, "maia_session"), Some("second".to_string()));
    }

    // ──── AuthEngine — 인증 해석 순서·인가 경계 ────
    //
    // 파일 기반 매니저만으로 엔진을 조립한다 (Qdrant 등 외부 인프라 불필요).

    struct Fixture {
        _tmp: TempDir,
        engine: AuthEngine,
        users: Arc<UserManager>,
        sessions: Arc<SessionManager>,
        api_keys: Arc<ApiKeyManager>,
        memberships: Arc<MembershipManager>,
        workspaces: Arc<WorkspaceManager>,
    }

    async fn setup(master_key: Option<&str>, dev_no_auth: bool) -> Fixture {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_str().unwrap();

        let users = Arc::new(UserManager::new(dir).await.unwrap());
        let sessions = Arc::new(SessionManager::new(dir).await.unwrap());
        let api_keys = Arc::new(ApiKeyManager::new(dir).await.unwrap());
        let memberships = Arc::new(MembershipManager::new(tmp.path()));
        let workspaces = Arc::new(WorkspaceManager::new(tmp.path()).await.unwrap());
        workspaces.ensure_default().await.unwrap();

        let engine = AuthEngine::new(
            master_key.map(|s| s.to_string()),
            dev_no_auth,
            users.clone(),
            sessions.clone(),
            api_keys.clone(),
            memberships.clone(),
            workspaces.clone(),
        );

        Fixture { _tmp: tmp, engine, users, sessions, api_keys, memberships, workspaces }
    }

    async fn add_workspace(f: &Fixture, id: &str) {
        f.workspaces
            .create(WorkspaceConfig::new(
                id.to_string(),
                id.to_string(),
                WorkspaceTemplate::Personal,
            ))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_no_credentials_rejected() {
        let f = setup(Some("master-secret"), false).await;
        assert!(f.engine.authenticate(None, None).await.is_none());
    }

    #[tokio::test]
    async fn test_master_key_bearer() {
        let f = setup(Some("master-secret"), false).await;
        let ctx = f.engine.authenticate(Some("master-secret"), None).await.unwrap();
        assert!(ctx.is_master);
        assert_eq!(ctx.source, AuthSource::Master);
        assert!(ctx.can_access_workspace("anything"));
    }

    #[tokio::test]
    async fn test_wrong_bearer_rejected() {
        let f = setup(Some("master-secret"), false).await;
        assert!(f.engine.authenticate(Some("wrong"), None).await.is_none());
    }

    #[tokio::test]
    async fn test_dev_mode_requires_explicit_opt_in() {
        // 회귀 방지 (기존 HIGH 이슈): 마스터키 미설정은 더 이상 fail-open이 아니다.
        // 자격증명 없는 요청은 마스터키가 없어도 거부되어야 한다.
        let f = setup(None, false).await;
        assert!(
            f.engine.authenticate(None, None).await.is_none(),
            "마스터키 미설정 + 옵트인 없음 = 잠금 유지 (fail-open 금지)"
        );

        // 명시적 MAIA_DEV_NO_AUTH=1일 때만 dev 컨텍스트가 열린다.
        let f = setup(None, true).await;
        let ctx = f.engine.authenticate(None, None).await.unwrap();
        assert_eq!(ctx.source, AuthSource::DevMode);
        assert!(ctx.is_master);
    }

    #[tokio::test]
    async fn test_registered_api_key_fixed_scope_regression() {
        // 기존 API 키 경로 회귀 방지: 키의 workspaces 목록 + 단일 권한 의미 유지.
        let f = setup(Some("master-secret"), false).await;
        let (_, raw) = f
            .api_keys
            .create_key(
                "k".to_string(),
                vec!["default".to_string()],
                Permission::ReadWrite,
                None,
                None,
            )
            .await
            .unwrap();

        let ctx = f.engine.authenticate(Some(&raw), None).await.unwrap();
        assert_eq!(ctx.source, AuthSource::ApiKey);
        assert!(!ctx.is_master);
        assert!(ctx.user.is_none(), "시스템 키는 계정 신원이 없다");
        assert!(ctx.can_access_workspace("default"));
        assert!(ctx.can_write_workspace("default"));
        assert!(!ctx.can_access_workspace("other"));
        assert_eq!(ctx.default_workspace(), "default");
    }

    #[tokio::test]
    async fn test_api_key_works_without_master_key() {
        // 마스터키가 없어도 정상 인증 체계는 동작해야 한다 (마스터키 = 선택적 부트스트랩).
        let f = setup(None, false).await;
        let (_, raw) = f
            .api_keys
            .create_key(
                "k".to_string(),
                vec!["default".to_string()],
                Permission::ReadOnly,
                None,
                None,
            )
            .await
            .unwrap();

        assert!(f.engine.authenticate(Some(&raw), None).await.is_some());
    }

    #[tokio::test]
    async fn test_session_cookie_authentication() {
        let f = setup(None, false).await;
        let user = f
            .users
            .create_user("alice", "password123", "Alice".to_string(), false)
            .await
            .unwrap();
        f.memberships
            .upsert_member("default", &user.user_id, Permission::ReadWrite)
            .await
            .unwrap();
        let (_, token) = f.sessions.create_session(&user.user_id).await.unwrap();

        let ctx = f.engine.authenticate(None, Some(&token)).await.unwrap();
        assert_eq!(ctx.source, AuthSource::Session);
        assert_eq!(ctx.user.as_ref().unwrap().username, "alice");
        assert!(ctx.can_write_workspace("default"));
        assert!(!ctx.is_admin(), "일반 계정은 시스템 admin이 아니다");
    }

    #[tokio::test]
    async fn test_invalid_bearer_falls_through_to_cookie() {
        // 해석 순서: Bearer 불일치 → 세션 쿠키 시도 (첫 "일치"가 승리).
        let f = setup(Some("master-secret"), false).await;
        let user = f
            .users
            .create_user("alice", "password123", "A".to_string(), false)
            .await
            .unwrap();
        let (_, token) = f.sessions.create_session(&user.user_id).await.unwrap();

        let ctx = f.engine.authenticate(Some("stale-key"), Some(&token)).await;
        assert!(ctx.is_some(), "무효 Bearer가 있어도 유효 세션 쿠키로 인증되어야 한다");
        assert_eq!(ctx.unwrap().source, AuthSource::Session);
    }

    #[tokio::test]
    async fn test_expired_or_bogus_session_rejected() {
        let f = setup(None, false).await;
        assert!(f.engine.authenticate(None, Some("bogus-token")).await.is_none());
    }

    #[tokio::test]
    async fn test_session_of_deleted_user_rejected() {
        let f = setup(None, false).await;
        let user = f
            .users
            .create_user("ghost", "password123", "G".to_string(), false)
            .await
            .unwrap();
        let (_, token) = f.sessions.create_session(&user.user_id).await.unwrap();
        f.users.delete_user(&user.user_id).await.unwrap();

        assert!(
            f.engine.authenticate(None, Some(&token)).await.is_none(),
            "계정이 삭제된 세션은 거부되어야 한다 (fail-closed)"
        );
    }

    #[tokio::test]
    async fn test_admin_user_gets_all_scope() {
        let f = setup(None, false).await;
        add_workspace(&f, "private-ws").await;
        let admin = f
            .users
            .create_user("root", "password123", "Root".to_string(), true)
            .await
            .unwrap();
        let (_, token) = f.sessions.create_session(&admin.user_id).await.unwrap();

        let ctx = f.engine.authenticate(None, Some(&token)).await.unwrap();
        assert!(ctx.is_admin());
        assert!(ctx.can_access_workspace("private-ws"), "글로벌 admin은 전 ws 접근");
        assert!(!ctx.is_master, "admin 계정은 마스터키가 아니다");
    }

    #[tokio::test]
    async fn test_user_access_is_membership_union_public() {
        let f = setup(None, false).await;
        add_workspace(&f, "member-ws").await;
        add_workspace(&f, "public-ws").await;
        add_workspace(&f, "private-ws").await;

        let user = f
            .users
            .create_user("bob", "password123", "Bob".to_string(), false)
            .await
            .unwrap();
        f.memberships
            .upsert_member("member-ws", &user.user_id, Permission::Admin)
            .await
            .unwrap();
        f.memberships
            .set_visibility("public-ws", WorkspaceVisibility::Public, Some(Permission::ReadOnly))
            .await
            .unwrap();

        let (_, token) = f.sessions.create_session(&user.user_id).await.unwrap();
        let ctx = f.engine.authenticate(None, Some(&token)).await.unwrap();

        // 멤버 ws: 자기 role
        assert!(ctx.is_workspace_admin("member-ws"));
        // public ws: public_permission (read_only → 쓰기 불가)
        assert!(ctx.can_access_workspace("public-ws"));
        assert!(!ctx.can_write_workspace("public-ws"));
        // private 비멤버: 접근 없음 + default(멤버 아님)도 접근 없음
        assert!(!ctx.can_access_workspace("private-ws"));
        assert!(!ctx.can_access_workspace("default"));
    }

    #[tokio::test]
    async fn test_public_toggle_takes_effect_immediately() {
        // 공개 → 비공개 전환이 다음 인증부터 즉시 반영되는지 (라이브 해석).
        let f = setup(None, false).await;
        add_workspace(&f, "toggle-ws").await;
        let user = f
            .users
            .create_user("bob", "password123", "B".to_string(), false)
            .await
            .unwrap();
        let (_, token) = f.sessions.create_session(&user.user_id).await.unwrap();

        f.memberships
            .set_visibility("toggle-ws", WorkspaceVisibility::Public, Some(Permission::ReadWrite))
            .await
            .unwrap();
        let ctx = f.engine.authenticate(None, Some(&token)).await.unwrap();
        assert!(ctx.can_write_workspace("toggle-ws"));

        f.memberships
            .set_visibility("toggle-ws", WorkspaceVisibility::Private, None)
            .await
            .unwrap();
        let ctx = f.engine.authenticate(None, Some(&token)).await.unwrap();
        assert!(!ctx.can_access_workspace("toggle-ws"), "private 전환 즉시 차단");
    }

    // ──── 소유 계정 있는 API 키 — 교집합 해석 ────

    #[tokio::test]
    async fn test_owned_key_intersects_key_scope_and_user_access() {
        let f = setup(None, false).await;
        add_workspace(&f, "ws-a").await;
        add_workspace(&f, "ws-b").await;
        add_workspace(&f, "ws-c").await;

        let user = f
            .users
            .create_user("carol", "password123", "C".to_string(), false)
            .await
            .unwrap();
        // 계정 접근: ws-a(read_write), ws-b(read_only). ws-c는 없음.
        f.memberships.upsert_member("ws-a", &user.user_id, Permission::ReadWrite).await.unwrap();
        f.memberships.upsert_member("ws-b", &user.user_id, Permission::ReadOnly).await.unwrap();

        // 키 스코프: ws-a, ws-c (ws-b는 키에 없음). 키 권한: ReadWrite.
        let (_, raw) = f
            .api_keys
            .create_key(
                "carol's key".to_string(),
                vec!["ws-a".to_string(), "ws-c".to_string()],
                Permission::ReadWrite,
                None,
                Some(user.user_id.clone()),
            )
            .await
            .unwrap();

        let ctx = f.engine.authenticate(Some(&raw), None).await.unwrap();
        assert_eq!(ctx.user.as_ref().unwrap().user_id, user.user_id);

        // 키∩계정 = ws-a만. 권한 = min(RW, RW) = RW.
        assert!(ctx.can_write_workspace("ws-a"));
        // ws-b: 계정은 접근하지만 키 스코프 밖 → 거부
        assert!(!ctx.can_access_workspace("ws-b"), "키 스코프 밖은 계정 접근권이 있어도 거부");
        // ws-c: 키 스코프지만 계정 접근권 없음 → 거부
        assert!(!ctx.can_access_workspace("ws-c"), "계정 접근권 밖은 키 스코프여도 거부");
        // 전역: 일반 계정 소유 키는 시스템 admin 불가
        assert!(!ctx.is_admin());
    }

    #[tokio::test]
    async fn test_owned_key_permission_capped_by_user_role() {
        // 키 권한이 계정 role보다 높아도 낮은 쪽으로 캡된다.
        let f = setup(None, false).await;
        add_workspace(&f, "ws-a").await;
        let user = f
            .users
            .create_user("dave", "password123", "D".to_string(), false)
            .await
            .unwrap();
        f.memberships.upsert_member("ws-a", &user.user_id, Permission::ReadOnly).await.unwrap();

        let (_, raw) = f
            .api_keys
            .create_key(
                "over-privileged".to_string(),
                vec!["ws-a".to_string()],
                Permission::ReadWrite,
                None,
                Some(user.user_id.clone()),
            )
            .await
            .unwrap();

        let ctx = f.engine.authenticate(Some(&raw), None).await.unwrap();
        assert!(ctx.can_access_workspace("ws-a"));
        assert!(
            !ctx.can_write_workspace("ws-a"),
            "min(키 RW, 계정 RO) = RO — 키가 계정보다 넓을 수 없다"
        );
    }

    #[tokio::test]
    async fn test_owned_key_loses_access_when_member_removed() {
        // 계정이 워크스페이스에서 빠지면 그 계정의 키도 그 즉시 접근 불가.
        let f = setup(None, false).await;
        add_workspace(&f, "ws-a").await;
        let user = f
            .users
            .create_user("erin", "password123", "E".to_string(), false)
            .await
            .unwrap();
        f.memberships.upsert_member("ws-a", &user.user_id, Permission::ReadWrite).await.unwrap();

        let (_, raw) = f
            .api_keys
            .create_key(
                "erin's key".to_string(),
                vec!["ws-a".to_string()],
                Permission::ReadWrite,
                None,
                Some(user.user_id.clone()),
            )
            .await
            .unwrap();

        assert!(f.engine.authenticate(Some(&raw), None).await.unwrap().can_access_workspace("ws-a"));

        f.memberships.remove_member("ws-a", &user.user_id).await.unwrap();
        let ctx = f.engine.authenticate(Some(&raw), None).await.unwrap();
        assert!(
            !ctx.can_access_workspace("ws-a"),
            "멤버십 제거 즉시 소유 키의 해당 ws 접근이 사라져야 한다"
        );
    }

    #[tokio::test]
    async fn test_owned_key_of_deleted_user_rejected() {
        let f = setup(None, false).await;
        let user = f
            .users
            .create_user("frank", "password123", "F".to_string(), false)
            .await
            .unwrap();
        let (_, raw) = f
            .api_keys
            .create_key(
                "orphan".to_string(),
                vec!["default".to_string()],
                Permission::ReadWrite,
                None,
                Some(user.user_id.clone()),
            )
            .await
            .unwrap();

        f.users.delete_user(&user.user_id).await.unwrap();
        assert!(
            f.engine.authenticate(Some(&raw), None).await.is_none(),
            "소유 계정이 사라진 키는 인증되면 안 된다 (fail-closed)"
        );
    }

    #[tokio::test]
    async fn test_owned_key_of_admin_user_keeps_key_scope() {
        // admin 계정 소유 키: 계정 접근권은 전체지만, 키 스코프가 상한이다.
        let f = setup(None, false).await;
        add_workspace(&f, "ws-a").await;
        add_workspace(&f, "ws-b").await;
        let admin = f
            .users
            .create_user("root", "password123", "R".to_string(), true)
            .await
            .unwrap();

        let (_, raw) = f
            .api_keys
            .create_key(
                "scoped admin key".to_string(),
                vec!["ws-a".to_string()],
                Permission::ReadWrite,
                None,
                Some(admin.user_id.clone()),
            )
            .await
            .unwrap();

        let ctx = f.engine.authenticate(Some(&raw), None).await.unwrap();
        assert!(ctx.can_write_workspace("ws-a"));
        assert!(
            !ctx.can_access_workspace("ws-b"),
            "admin 계정 키라도 키 스코프(교집합 상한)를 넘을 수 없다"
        );
        assert!(!ctx.is_admin(), "min(키 RW, 계정 Admin) = RW — 전역 admin 아님");
    }
}
