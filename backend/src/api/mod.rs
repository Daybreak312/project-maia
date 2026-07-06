mod ingest;
mod search;
mod documents;
mod workspaces;
mod keys;
pub mod settings;

pub use ingest::ingest_handler;
pub use search::search_handler;
pub use documents::{get_document_handler, recent_handler, update_document_handler, delete_document_handler, reindex_handler};
pub use workspaces::{
    list_workspaces_handler, create_workspace_handler, get_workspace_handler,
    delete_workspace_handler,
};
pub use keys::{list_keys_handler, create_key_handler, revoke_key_handler};

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

    if !state.workspaces.exists(&ws).await {
        return Err((
            StatusCode::NOT_FOUND,
            format!("Workspace '{}' not found", ws),
        ));
    }

    if !ctx.can_access_workspace(&ws) {
        return Err((
            StatusCode::FORBIDDEN,
            format!("API key does not have access to workspace '{}'", ws),
        ));
    }

    Ok(ws)
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

/// 쓰기 권한(read_write 이상)을 요구한다. 실패 시 403.
pub fn require_write(ctx: &AuthContext) -> Result<(), (StatusCode, String)> {
    if ctx.can_write() {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            "Write permission required".to_string(),
        ))
    }
}
