//! Patrol·거버넌스 API (Phase 5) — 실행/이력, Review Queue 조회·판단, 피드백, 메트릭.
//!
//! 모든 핸들러는 `AppState.patrol` 파사드를 경유한다(저장소를 직접 만지지 않음). 권한:
//! 조회(이력·큐·메트릭)는 워크스페이스 접근이면 되고, 상태를 바꾸는 실행·판단·피드백은
//! write 이상을 요구한다(판단은 문서 삭제까지 가능하므로 read_only에 열지 않는다).

use axum::{
    extract::{Query, State},
    http::StatusCode,
    Extension, Json,
};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::api::{require_write, resolve_and_authorize_workspace, WorkspaceQuery};
use crate::auth::AuthContext;
use crate::patrol::detectors::DetectorKind;
use crate::patrol::history::PatrolRun;
use crate::patrol::review::{ReviewDecision, ReviewItem, ReviewStatus};
use crate::AppState;

/// 쿼리 문자열을 snake_case 열거형으로 관대하게 파싱한다(serde 재사용).
/// 값이 없으면 None, 알 수 없는 값도 None(필터 무시).
fn parse_enum<T: serde::de::DeserializeOwned>(s: &Option<String>) -> Option<T> {
    let raw = s.as_ref()?;
    serde_json::from_value(serde_json::Value::String(raw.clone())).ok()
}

// ──── Patrol 실행 / 이력 ────

/// POST /api/patrol/run?workspace= — 수동 트리거(write). 동기 실행 후 리포트 반환.
///
/// 개인 규모에서 즉시 피드백(탐지 수)을 주기 위해 동기 실행한다. 실행 이력도 남는다.
pub async fn run_patrol_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Query(wq): Query<WorkspaceQuery>,
) -> Result<Json<PatrolRun>, (StatusCode, String)> {
    let workspace = resolve_and_authorize_workspace(&state, &ctx, wq.workspace).await?;
    require_write(&ctx, &workspace)?;

    state
        .patrol
        .run(&workspace, "manual", Utc::now())
        .await
        .map(Json)
        .map_err(|e| {
            tracing::error!("Patrol 실행 실패: {e:?}");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })
}

/// GET /api/patrol/history?workspace= — 실행 이력·마지막 실행 시각(워크스페이스 접근).
pub async fn patrol_history_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Query(wq): Query<WorkspaceQuery>,
) -> Result<Json<crate::patrol::history::PatrolState>, (StatusCode, String)> {
    let workspace = resolve_and_authorize_workspace(&state, &ctx, wq.workspace).await?;
    state
        .patrol
        .history(&workspace)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

// ──── Review Queue ────

#[derive(Debug, Deserialize)]
pub struct ReviewListQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    /// 상태 필터: pending/valid/needs_fix/deleted/dismissed.
    #[serde(default)]
    pub status: Option<String>,
    /// 유형 필터: staleness/duplicate/orphan/external_mismatch.
    #[serde(default)]
    pub kind: Option<String>,
}

/// GET /api/review?workspace=&status=&kind= — Review Queue 조회(워크스페이스 접근).
pub async fn list_reviews_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Query(q): Query<ReviewListQuery>,
) -> Result<Json<Vec<ReviewItem>>, (StatusCode, String)> {
    let workspace = resolve_and_authorize_workspace(&state, &ctx, q.workspace.clone()).await?;
    let status: Option<ReviewStatus> = parse_enum(&q.status);
    let kind: Option<DetectorKind> = parse_enum(&q.kind);

    state
        .patrol
        .list_reviews(&workspace, status, kind)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

#[derive(Debug, Deserialize)]
pub struct JudgeRequest {
    /// 판단할 항목 ID들(단건도 리스트로 — 단일/일괄 통합 경로).
    pub ids: Vec<Uuid>,
    /// 판단: valid/needs_fix/deleted/dismissed.
    pub decision: ReviewDecision,
}

#[derive(Debug, Serialize)]
pub struct JudgeResponse {
    /// 판단이 반영된(존재한) 항목들.
    pub items: Vec<ReviewItem>,
}

/// POST /api/review/judge?workspace= — 항목 판단(write). 단건·일괄 모두 이 경로.
///
/// 유효→freshness 갱신, 삭제→복구 가능 삭제. 멱등(같은 판단 재제출이 상태를 깨지 않음).
pub async fn judge_reviews_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Query(wq): Query<WorkspaceQuery>,
    Json(req): Json<JudgeRequest>,
) -> Result<Json<JudgeResponse>, (StatusCode, String)> {
    let workspace = resolve_and_authorize_workspace(&state, &ctx, wq.workspace).await?;
    require_write(&ctx, &workspace)?;

    state
        .patrol
        .judge_bulk(&workspace, &req.ids, req.decision, Utc::now())
        .await
        .map(|items| Json(JudgeResponse { items }))
        .map_err(|e| {
            tracing::error!("Review 판단 실패: {e:?}");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })
}

// ──── 피드백 ────

#[derive(Debug, Deserialize)]
pub struct FeedbackRequest {
    /// 피드백이 발생한 검색 쿼리.
    pub query: String,
    /// 관련 없다고 표시된 문서.
    pub document_id: Uuid,
}

/// POST /api/feedback?workspace= — 검색 결과 "관련 없음" 피드백(write). 실패 무해.
pub async fn submit_feedback_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Query(wq): Query<WorkspaceQuery>,
    Json(req): Json<FeedbackRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let workspace = resolve_and_authorize_workspace(&state, &ctx, wq.workspace).await?;
    require_write(&ctx, &workspace)?;

    state
        .patrol
        .record_feedback(&workspace, req.query, req.document_id, Utc::now())
        .await;
    Ok(StatusCode::NO_CONTENT)
}

// ──── 메트릭 ────

#[derive(Debug, Deserialize)]
pub struct MetricsQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    /// 시작 일자 `YYYY-MM-DD`(미지정 시 30일 전).
    #[serde(default)]
    pub from: Option<String>,
    /// 종료 일자 `YYYY-MM-DD`(미지정 시 오늘).
    #[serde(default)]
    pub until: Option<String>,
}

/// GET /api/metrics?workspace=&from=&until= — 기간 메트릭 롤업(워크스페이스 접근).
pub async fn metrics_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Query(q): Query<MetricsQuery>,
) -> Result<Json<Vec<crate::patrol::metrics::DailyRollup>>, (StatusCode, String)> {
    let workspace = resolve_and_authorize_workspace(&state, &ctx, q.workspace.clone()).await?;
    let today = Utc::now();
    let until = q.until.unwrap_or_else(|| today.format("%Y-%m-%d").to_string());
    let from = q
        .from
        .unwrap_or_else(|| (today - Duration::days(30)).format("%Y-%m-%d").to_string());

    state
        .patrol
        .metrics_range(&workspace, &from, &until)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}
