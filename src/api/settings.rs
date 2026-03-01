use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

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

/// PUT /api/settings - 설정 변경
pub async fn update_settings(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UpdateSettingsRequest>,
) -> Result<Json<SettingsResponse>, (StatusCode, String)> {
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

/// POST /api/settings/models/{provider}/key - API Key 설정
pub async fn set_api_key(
    State(state): State<Arc<AppState>>,
    Path(provider): Path<ProviderType>,
    Json(req): Json<SetApiKeyRequest>,
) -> Result<Json<SettingsResponse>, (StatusCode, String)> {
    state.settings.set_api_key(provider, req.api_key).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let settings = state.settings.get().await;
    Ok(Json(settings.to_response()))
}

/// DELETE /api/settings/models/{provider}/key - API Key 삭제
pub async fn delete_api_key(
    State(state): State<Arc<AppState>>,
    Path(provider): Path<ProviderType>,
) -> Result<Json<SettingsResponse>, (StatusCode, String)> {
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

/// POST /api/settings/models/{provider}/test - API Key 유효성 검증
pub async fn test_api_key(
    State(state): State<Arc<AppState>>,
    Path(provider): Path<ProviderType>,
) -> Result<Json<TestApiKeyResponse>, (StatusCode, String)> {
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
