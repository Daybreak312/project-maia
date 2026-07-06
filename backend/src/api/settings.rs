use axum::{
    extract::{Path, State},
    http::StatusCode,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::api::require_admin;
use crate::auth::AuthContext;
use crate::llm::ProviderType;
use crate::settings::{SettingsManager, SettingsResponse};
use crate::AppState;

/// GET /api/settings - 현재 설정 조회
pub async fn get_settings(
    State(state): State<Arc<AppState>>,
) -> Json<SettingsResponse> {
    let settings = state.settings.get().await;
    Json(settings.to_response())
}

/// 설정 업데이트 요청
#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    #[serde(default)]
    pub parsing_provider: Option<ProviderType>,
    #[serde(default)]
    pub embedding_provider: Option<ProviderType>,
}

/// PUT /api/settings - 설정 변경 (admin)
pub async fn update_settings(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Json(req): Json<UpdateSettingsRequest>,
) -> Result<Json<SettingsResponse>, (StatusCode, String)> {
    require_admin(&ctx)?;
    if let Some(provider) = req.parsing_provider {
        // 해당 provider의 API Key가 있는지 확인
        if !state.settings.is_provider_available(provider).await {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("API key for {} is not configured", provider),
            ));
        }
        state.settings.set_parsing_provider(provider).await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    if let Some(provider) = req.embedding_provider {
        if !state.settings.is_provider_available(provider).await {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("API key for {} is not configured", provider),
            ));
        }
        state.settings.set_embedding_provider(provider).await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    let settings = state.settings.get().await;
    Ok(Json(settings.to_response()))
}

/// API Key 설정 요청
#[derive(Debug, Deserialize)]
pub struct SetApiKeyRequest {
    pub api_key: String,
}

/// POST /api/settings/models/{provider}/key - API Key 설정 (admin)
pub async fn set_api_key(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Path(provider): Path<ProviderType>,
    Json(req): Json<SetApiKeyRequest>,
) -> Result<Json<SettingsResponse>, (StatusCode, String)> {
    require_admin(&ctx)?;
    state.settings.set_api_key(provider, req.api_key).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let settings = state.settings.get().await;
    Ok(Json(settings.to_response()))
}

/// DELETE /api/settings/models/{provider}/key - API Key 삭제 (admin)
pub async fn delete_api_key(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Path(provider): Path<ProviderType>,
) -> Result<Json<SettingsResponse>, (StatusCode, String)> {
    require_admin(&ctx)?;
    state.settings.remove_api_key(provider).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let settings = state.settings.get().await;
    Ok(Json(settings.to_response()))
}

/// API Key 테스트 응답
#[derive(Debug, Serialize)]
pub struct TestApiKeyResponse {
    pub valid: bool,
    pub message: String,
}

/// POST /api/settings/models/{provider}/test - API Key 유효성 검증 (admin)
///
/// 저장된 provider 키로 외부 validate 호출을 유발하므로(비용/레이트리밋 소진),
/// 설정 mutating 형제 엔드포인트와 동일하게 admin 권한을 요구한다.
pub async fn test_api_key(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Path(provider): Path<ProviderType>,
) -> Result<Json<TestApiKeyResponse>, (StatusCode, String)> {
    require_admin(&ctx)?;
    let api_key = state.settings.get_api_key(provider).await
        .ok_or((StatusCode::BAD_REQUEST, "API key not configured".to_string()))?;

    let llm_provider = crate::llm::create_llm_provider(provider, api_key);

    match llm_provider.validate_api_key().await {
        Ok(true) => Ok(Json(TestApiKeyResponse {
            valid: true,
            message: "API key is valid".to_string(),
        })),
        Ok(false) => Ok(Json(TestApiKeyResponse {
            valid: false,
            message: "API key is invalid".to_string(),
        })),
        Err(e) => Ok(Json(TestApiKeyResponse {
            valid: false,
            message: format!("Validation failed: {}", e),
        })),
    }
}
