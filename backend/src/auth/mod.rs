pub mod keys;
pub mod sessions;
pub mod users;

pub use keys::{
    ApiKey, ApiKeyManager, AuthContext, Permission,
    generate_key_id, generate_raw_key, hash_key,
};
pub use sessions::{Session, SessionManager, SESSION_COOKIE_NAME};
pub use users::{User, UserManager};

use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};

use crate::AppState;

/// Authorization 헤더에서 Bearer 토큰을 추출한다.
fn extract_bearer(req: &Request<Body>) -> Option<String> {
    req.headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(|s| s.to_string())
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

/// API 키 인증 미들웨어.
///
/// 해석 순서: 마스터키(전체 admin) → ApiKeyManager 조회(스코프 적용) → 401.
/// `MAIA_API_KEY` 미설정 시 인증을 건너뛴다 (로컬 개발 모드).
///
/// 검증에 성공하면 `AuthContext`를 request extension에 주입해 하위 핸들러가
/// 워크스페이스 접근·권한을 판단할 수 있게 한다. 등록된 키로 인증되면
/// `last_used_at`을 비블로킹(tokio::spawn)으로 갱신해 응답 지연을 피한다.
pub async fn require_api_key(
    State(state): State<Arc<AppState>>,
    mut req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    // 개발 모드: 마스터키 미설정 → 인증 비활성 (dev 컨텍스트 주입)
    let Some(master) = &state.api_key else {
        req.extensions_mut().insert(AuthContext::dev_mode());
        return Ok(next.run(req).await);
    };

    let Some(token) = extract_bearer(&req) else {
        return Err(StatusCode::UNAUTHORIZED);
    };

    // 1) 마스터키 (상수 시간 비교)
    if constant_time_eq(token.as_bytes(), master.as_bytes()) {
        req.extensions_mut().insert(AuthContext::master());
        return Ok(next.run(req).await);
    }

    // 2) 등록된 API 키 (해시 조회 + 만료 체크)
    if let Some(key) = state.api_keys.authenticate(&token).await {
        let ctx = AuthContext::from_api_key(&key);

        // last_used_at 비블로킹 갱신 (요청 경로 블로킹 금지)
        let manager = state.api_keys.clone();
        let key_id = key.key_id.clone();
        tokio::spawn(async move {
            if let Err(e) = manager.update_last_used(&key_id).await {
                tracing::warn!("Failed to update last_used_at for {}: {}", key_id, e);
            }
        });

        req.extensions_mut().insert(ctx);
        return Ok(next.run(req).await);
    }

    // 3) 어느 것과도 불일치
    Err(StatusCode::UNAUTHORIZED)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
