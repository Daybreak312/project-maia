use axum::{
    extract::{Query, State},
    http::StatusCode,
    Extension, Json,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::api::{require_write, resolve_and_authorize_workspace};
use crate::auth::AuthContext;
use crate::models::api::{IngestOutcome, IngestRequest};
use crate::AppState;

/// ingest 전용 쿼리 파라미터.
/// - `workspace`: 대상 워크스페이스 (미지정 시 키 기본값)
/// - `mode`: "raw"이면 에이전트 판단을 우회해 기존과 동일하게 저장. 그 외/미지정은
///   에이전트 모드(신규/업데이트/분할/중복 판단).
#[derive(Debug, Deserialize)]
pub struct IngestQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
}

pub async fn ingest_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Query(q): Query<IngestQuery>,
    Json(req): Json<IngestRequest>,
) -> Result<Json<IngestOutcome>, (StatusCode, String)> {
    let workspace = resolve_and_authorize_workspace(&state, &ctx, q.workspace).await?;
    require_write(&ctx, &workspace)?;

    let is_raw = q.mode.as_deref() == Some("raw");

    let result = if is_raw {
        // 에이전트 우회 — 기존 인덱싱 파이프라인 그대로. 의도된 우회이므로 fallback=false.
        state
            .indexer
            .ingest_to_workspace(req.content, &workspace)
            .await
            .map(|resp| {
                let id = resp.id;
                IngestOutcome::from_response(resp, "raw", vec![id], 0, false, "raw 모드 (에이전트 우회)")
            })
    } else {
        // 에이전트 모드 — 판단 후 전략 실행. 내부에서 실패 시 raw로 폴백(fallback=true).
        state
            .indexer
            .smart_ingest_to_workspace(req.content, &workspace)
            .await
    };

    result.map(Json).map_err(|e| {
        tracing::error!("Ingest failed: {e:?}");
        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })
}
