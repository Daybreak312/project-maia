pub mod keys;

pub use keys::{
    ApiKey, ApiKeyManager, AuthContext, Permission,
    generate_key_id, generate_raw_key, hash_key,
};

use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};

use crate::AppState;

/// API 키 인증 미들웨어.
///
/// `MAIA_API_KEY`가 설정되어 있으면 모든 요청에 `Authorization: Bearer <key>` 헤더를 요구한다.
/// 설정되어 있지 않으면 인증을 건너뛴다 (로컬 개발 환경).
pub async fn require_api_key(
    State(state): State<Arc<AppState>>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let Some(expected) = &state.api_key else {
        return Ok(next.run(req).await);
    };

    let authorized = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|h| {
            h.strip_prefix("Bearer ")
                .is_some_and(|token| token == expected.as_str())
        });

    if authorized {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}
