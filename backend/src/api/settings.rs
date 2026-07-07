use axum::{
    extract::{Path, State},
    http::StatusCode,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use std::path::Path as FsPath;
use std::sync::Arc;

use crate::api::require_admin;
use crate::auth::AuthContext;
use crate::llm::{parse_codex_auth_json, ProviderType};
use crate::settings::SettingsResponse;
use crate::AppState;

/// 로컬 임베딩 모델 캐시가 준비됐는지 근사한다(디렉토리 존재 + 비어있지 않음).
fn models_cache_ready(dir: &FsPath) -> bool {
    std::fs::read_dir(dir)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
}

/// 현재 설정을 API 응답으로 변환한다(로컬 캐시 준비 여부 포함).
async fn settings_response(state: &AppState) -> SettingsResponse {
    let settings = state.settings.get().await;
    let cache_ready = models_cache_ready(&state.settings.models_dir());
    settings.to_response(cache_ready)
}

/// provider가 현재 사용 가능한지(파싱/임베딩 선택 가능 조건의 "설정됨" 부분).
///
/// - `local`: 키 불요 → 항상 사용 가능.
/// - `codex`: auth.json 임포트 여부.
/// - 그 외: API 키 등록 여부.
async fn is_provider_configured(state: &AppState, provider: ProviderType) -> bool {
    match provider {
        ProviderType::Local => true,
        ProviderType::Codex => state.settings.get_codex_auth().await.is_some(),
        key_based => state.settings.is_provider_available(key_based).await,
    }
}

/// GET /api/settings - 현재 설정 조회
pub async fn get_settings(State(state): State<Arc<AppState>>) -> Json<SettingsResponse> {
    Json(settings_response(&state).await)
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
///
/// 교차 선택 제약(FR5): `local`은 파싱 provider로, `codex`는 임베딩 provider로
/// 선택할 수 없다(400 + 명확한 메시지). 선택 provider가 미설정이어도 400.
pub async fn update_settings(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Json(req): Json<UpdateSettingsRequest>,
) -> Result<Json<SettingsResponse>, (StatusCode, String)> {
    require_admin(&ctx)?;

    if let Some(provider) = req.parsing_provider {
        if !provider.valid_for_parsing() {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("{provider}는 파싱 provider로 사용할 수 없습니다 (임베딩 전용)"),
            ));
        }
        if !is_provider_configured(&state, provider).await {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("{provider}가 설정되지 않았습니다 (키/토큰을 먼저 등록하세요)"),
            ));
        }
        state
            .settings
            .set_parsing_provider(provider)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    if let Some(provider) = req.embedding_provider {
        if !provider.valid_for_embedding() {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("{provider}는 임베딩 provider로 사용할 수 없습니다"),
            ));
        }
        if !is_provider_configured(&state, provider).await {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("{provider}가 설정되지 않았습니다"),
            ));
        }
        state
            .settings
            .set_embedding_provider(provider)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    Ok(Json(settings_response(&state).await))
}

/// API Key 설정 요청
#[derive(Debug, Deserialize)]
pub struct SetApiKeyRequest {
    pub api_key: String,
}

/// POST /api/settings/models/{provider}/key - API Key 설정 (admin)
///
/// claude/gemini/openai 전용. codex는 import 전용, local은 키 불요라 400.
pub async fn set_api_key(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Path(provider): Path<ProviderType>,
    Json(req): Json<SetApiKeyRequest>,
) -> Result<Json<SettingsResponse>, (StatusCode, String)> {
    require_admin(&ctx)?;
    if !provider.uses_api_key() {
        let msg = match provider {
            ProviderType::Codex => {
                "codex는 키가 아니라 auth.json 임포트를 사용하세요 (POST /api/settings/models/codex/import)"
            }
            ProviderType::Local => "local 임베딩은 키가 필요 없습니다",
            _ => "이 provider는 키를 설정할 수 없습니다",
        };
        return Err((StatusCode::BAD_REQUEST, msg.to_string()));
    }
    state
        .settings
        .set_api_key(provider, req.api_key)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(settings_response(&state).await))
}

/// DELETE /api/settings/models/{provider}/key - API Key 삭제 (admin)
pub async fn delete_api_key(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Path(provider): Path<ProviderType>,
) -> Result<Json<SettingsResponse>, (StatusCode, String)> {
    require_admin(&ctx)?;
    state
        .settings
        .remove_api_key(provider)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(settings_response(&state).await))
}

/// POST /api/settings/models/codex/import - Codex auth.json 임포트 (admin)
///
/// body는 `~/.codex/auth.json` 원문 객체, 또는 `{ "auth_json": "<원문 문자열>" }`
/// 래퍼를 모두 허용한다(붙여넣기 UX). 재임포트는 덮어쓰기.
pub async fn import_codex(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<SettingsResponse>, (StatusCode, String)> {
    require_admin(&ctx)?;

    // 래퍼({auth_json}) 또는 원문 객체 모두 지원.
    let raw = match body.get("auth_json").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => body.to_string(),
    };

    let auth = parse_codex_auth_json(&raw).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    state
        .settings
        .set_codex_auth(auth)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(settings_response(&state).await))
}

/// API Key 테스트 응답
#[derive(Debug, Serialize)]
pub struct TestApiKeyResponse {
    pub valid: bool,
    pub message: String,
}

/// POST /api/settings/models/{provider}/test - provider 유효성 검증 (admin)
///
/// provider별 검증 경로(FR3/FR5):
/// - `local`: 모델 로드 + 1회 임베딩 + 차원 확인(임베딩 provider).
/// - `codex`: refresh 포함 소형 responses 핑(LLM provider).
/// - claude(oat 포함)/gemini/openai: 기존 키 유효성 핑(LLM provider).
///
/// 외부 호출을 유발하므로(비용/레이트리밋) mutating 형제와 동일하게 admin 요구.
pub async fn test_api_key(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Path(provider): Path<ProviderType>,
) -> Result<Json<TestApiKeyResponse>, (StatusCode, String)> {
    require_admin(&ctx)?;

    // local은 임베딩 provider, 그 외는 LLM provider 경로로 검증한다.
    let result = if provider == ProviderType::Local {
        match state.indexer.embedding_provider_for(provider).await {
            Ok(p) => p.validate_api_key().await,
            Err(e) => Err(e),
        }
    } else {
        match state.indexer.llm_provider_for(provider).await {
            Ok(p) => p.validate_api_key().await,
            Err(e) => Err(e),
        }
    };

    Ok(Json(match result {
        Ok(true) => TestApiKeyResponse {
            valid: true,
            message: "검증 성공".to_string(),
        },
        Ok(false) => TestApiKeyResponse {
            valid: false,
            message: "검증 실패 (유효하지 않음)".to_string(),
        },
        Err(e) => TestApiKeyResponse {
            valid: false,
            message: format!("검증 실패: {e}"),
        },
    }))
}
