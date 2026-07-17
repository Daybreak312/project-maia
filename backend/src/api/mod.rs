mod ingest;
mod search;
mod documents;
mod graph;
mod workspaces;
mod connectors;
mod keys;
mod patrol;
mod users;
pub mod settings;

pub use ingest::ingest_handler;
pub use search::search_handler;
pub use documents::{get_document_handler, recent_handler, update_document_handler, delete_document_handler, reindex_handler};
pub use graph::{neighbors_handler, add_edge_handler, remove_edge_handler};
pub use workspaces::{
    list_workspaces_handler, create_workspace_handler, get_workspace_handler,
    delete_workspace_handler, update_workspace_patrol_handler,
};
pub use connectors::{
    connector_status_handler, delete_connector_handler, list_connectors_handler,
    register_connector_handler, trigger_connector_handler,
};
pub use keys::{list_keys_handler, create_key_handler, revoke_key_handler};
pub use users::{
    change_password_handler, create_user_handler, delete_user_handler, list_users_handler,
    UserInfo,
};
pub use patrol::{
    judge_reviews_handler, list_reviews_handler, metrics_handler, patrol_history_handler,
    run_patrol_handler, submit_feedback_handler,
};

use axum::http::StatusCode;
use serde::Deserialize;

use crate::auth::AuthContext;
use crate::AppState;

/// 모든 문서/검색/인제스트 엔드포인트가 공유하는 워크스페이스 지정 쿼리 파라미터.
/// 예: `POST /ingest?workspace=work`
#[derive(Debug, Deserialize)]
pub struct WorkspaceQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

/// 요청의 대상 워크스페이스를 결정하고 접근 권한·존재 여부를 검증한다.
///
/// 해석 규칙:
/// - 명시된 `workspace` 파라미터가 있으면 그것을,
/// - 없으면 키에 바인딩된 기본 워크스페이스(마스터/개발모드는 `default`)를 사용.
///
/// 검증:
/// - 존재하지 않는 워크스페이스 → 404
/// - 접근 권한 없는 워크스페이스 → 403
pub async fn resolve_and_authorize_workspace(
    state: &AppState,
    ctx: &AuthContext,
    requested: Option<String>,
) -> Result<String, (StatusCode, String)> {
    let ws = requested
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| ctx.default_workspace());

    let exists = state.workspaces.exists(&ws).await;
    let can_access = ctx.can_access_workspace(&ws);
    authorize_workspace_access(&ws, exists, can_access)?;

    Ok(ws)
}

/// 워크스페이스 접근 결정 (순수 함수, 테스트 가능).
///
/// 존재/접근 두 사실로부터 결과를 결정한다:
/// - 존재하지 않음 → 404
/// - 존재하나 접근 불가 → 403 (PRD 인수 조건: 타 워크스페이스 접근 시 403)
///
/// 존재 확인을 접근 확인보다 먼저 한다. 신뢰 공유 모델(개인+소규모)이라 워크스페이스
/// 존재 여부 노출은 수용하며, PRD가 명시적으로 403을 요구하므로 접근 실패를 404로
/// 뭉개지 않는다.
fn authorize_workspace_access(
    ws: &str,
    exists: bool,
    can_access: bool,
) -> Result<(), (StatusCode, String)> {
    if !exists {
        return Err((
            StatusCode::NOT_FOUND,
            format!("Workspace '{}' not found", ws),
        ));
    }

    if !can_access {
        return Err((
            StatusCode::FORBIDDEN,
            format!("API key does not have access to workspace '{}'", ws),
        ));
    }

    Ok(())
}

/// admin 권한을 요구한다. 실패 시 403.
pub fn require_admin(ctx: &AuthContext) -> Result<(), (StatusCode, String)> {
    if ctx.is_admin() {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            "Admin permission required".to_string(),
        ))
    }
}

/// 대상 워크스페이스에 대한 쓰기 권한(read_write 이상)을 요구한다. 실패 시 403.
///
/// 전역 권한이 아니라 "그 워크스페이스에 대한" 유효 권한으로 판정한다 —
/// 유저 세션은 워크스페이스마다 role이 다를 수 있기 때문. 호출부는 반드시
/// `resolve_and_authorize_workspace`로 대상을 확정한 뒤 호출한다.
pub fn require_write(ctx: &AuthContext, workspace_id: &str) -> Result<(), (StatusCode, String)> {
    if ctx.can_write_workspace(workspace_id) {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            format!("Write permission required for workspace '{}'", workspace_id),
        ))
    }
}

/// 워크스페이스 관리 권한을 요구한다: 글로벌 admin(마스터키·admin 계정·admin 키)
/// 또는 해당 워크스페이스 role=admin 멤버. 실패 시 403.
/// (멤버십·공개 설정 변경 등 워크스페이스 거버넌스 작업의 관문)
pub fn require_workspace_admin(
    ctx: &AuthContext,
    workspace_id: &str,
) -> Result<(), (StatusCode, String)> {
    if ctx.is_admin() || ctx.is_workspace_admin(workspace_id) {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            format!("Admin permission required for workspace '{}'", workspace_id),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::keys::{AuthSource, WorkspaceScope};
    use crate::auth::Permission;
    use std::collections::BTreeMap;

    fn ctx(perm: Permission) -> AuthContext {
        AuthContext {
            key_id: "t".to_string(),
            permissions: perm,
            scope: WorkspaceScope::Fixed(vec!["default".to_string()]),
            is_master: false,
            source: AuthSource::ApiKey,
            user: None,
        }
    }

    /// 워크스페이스별 role이 다른 유저 세션 컨텍스트
    fn user_ctx(map: &[(&str, Permission)]) -> AuthContext {
        let scope: BTreeMap<String, Permission> = map
            .iter()
            .map(|(w, p)| (w.to_string(), p.clone()))
            .collect();
        AuthContext {
            key_id: "user_test".to_string(),
            permissions: Permission::ReadOnly,
            scope: WorkspaceScope::PerWorkspace(scope),
            is_master: false,
            source: AuthSource::Session,
            user: None,
        }
    }

    #[test]
    fn test_require_admin_only_admin_passes() {
        assert!(require_admin(&ctx(Permission::Admin)).is_ok());
        assert!(require_admin(&ctx(Permission::ReadWrite)).is_err());
        assert!(require_admin(&ctx(Permission::ReadOnly)).is_err());
        // 마스터키는 admin
        assert!(require_admin(&AuthContext::master()).is_ok());
    }

    #[test]
    fn test_require_admin_returns_403() {
        let (status, _) = require_admin(&ctx(Permission::ReadOnly)).unwrap_err();
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[test]
    fn test_require_write_readwrite_and_admin_pass() {
        // 기존 API 키(Fixed 스코프) 회귀: 스코프 내 워크스페이스에 대해
        // 키의 단일 권한으로 판정된다.
        assert!(require_write(&ctx(Permission::ReadWrite), "default").is_ok());
        assert!(require_write(&ctx(Permission::Admin), "default").is_ok());
        assert!(require_write(&ctx(Permission::ReadOnly), "default").is_err());
    }

    #[test]
    fn test_require_write_returns_403() {
        let (status, _) = require_write(&ctx(Permission::ReadOnly), "default").unwrap_err();
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[test]
    fn test_require_write_out_of_scope_denied() {
        // 쓰기 권한이 있어도 스코프 밖 워크스페이스는 거부 (fail-closed).
        // 정상 흐름에선 resolve가 먼저 403을 내지만, 이 함수 단독으로도 안전해야 한다.
        assert!(require_write(&ctx(Permission::ReadWrite), "other-ws").is_err());
    }

    #[test]
    fn test_require_write_per_workspace_roles() {
        // 유저 세션: 워크스페이스마다 다른 role — ws별로 판정이 달라야 한다.
        let ctx = user_ctx(&[("ws-rw", Permission::ReadWrite), ("ws-ro", Permission::ReadOnly)]);
        assert!(require_write(&ctx, "ws-rw").is_ok());
        assert!(require_write(&ctx, "ws-ro").is_err());
        assert!(require_write(&ctx, "ws-none").is_err());
    }

    #[test]
    fn test_require_workspace_admin_paths() {
        // 글로벌 admin(마스터키)은 모든 워크스페이스 관리 가능
        assert!(require_workspace_admin(&AuthContext::master(), "any").is_ok());
        // admin 권한 API 키(전역 admin 의미 유지 — 기존 require_admin과 동일 모델)
        assert!(require_workspace_admin(&ctx(Permission::Admin), "elsewhere").is_ok());

        // 해당 ws role=admin 멤버는 그 ws만 관리 가능
        let ws_admin = user_ctx(&[("mine", Permission::Admin), ("other", Permission::ReadWrite)]);
        assert!(require_workspace_admin(&ws_admin, "mine").is_ok());
        assert!(require_workspace_admin(&ws_admin, "other").is_err());
        assert!(require_workspace_admin(&ws_admin, "stranger").is_err());

        // 일반 멤버는 불가
        let member = user_ctx(&[("mine", Permission::ReadWrite)]);
        let (status, _) = require_workspace_admin(&member, "mine").unwrap_err();
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    // ──── authorize_workspace_access (403/404 매핑) ────

    #[test]
    fn test_authorize_workspace_access_ok() {
        assert!(authorize_workspace_access("work", true, true).is_ok());
    }

    #[test]
    fn test_authorize_workspace_access_not_found() {
        // 존재하지 않는 워크스페이스 → 404
        let (status, _) = authorize_workspace_access("ghost", false, false).unwrap_err();
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_authorize_workspace_access_forbidden() {
        // 존재하지만 접근 불가 → 403 (PRD 인수 조건: 타 워크스페이스 접근 403).
        // personal 스코프 키로 work 문서 접근 시나리오의 결정 로직.
        let (status, _) = authorize_workspace_access("personal", true, false).unwrap_err();
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
}
