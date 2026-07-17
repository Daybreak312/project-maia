use axum::{
    extract::{Path, State},
    http::StatusCode,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::api::require_workspace_admin;
use crate::auth::{AuthContext, Permission, UserManager};
use crate::workspace::{
    MembershipManager, WorkspaceManager, WorkspaceMembers, WorkspaceVisibility,
};
use crate::AppState;

/// 멤버 목록 응답의 멤버 항목 — 계정 정보를 함께 실어 UI가 재조회하지 않게 한다.
/// 계정이 삭제된 잔존 항목은 username/display_name이 null로 표시된다.
#[derive(Debug, Serialize)]
pub struct MemberView {
    pub user_id: String,
    pub username: Option<String>,
    pub display_name: Option<String>,
    pub role: Permission,
}

/// GET /api/workspaces/:id/members 응답
#[derive(Debug, Serialize)]
pub struct MembersResponse {
    pub visibility: WorkspaceVisibility,
    pub public_permission: Permission,
    pub members: Vec<MemberView>,
}

/// 멤버 추가/변경 요청 (role 지정 = 초대. 수락 플로우 없음)
#[derive(Debug, Deserialize)]
pub struct UpsertMemberRequest {
    pub role: Permission,
}

/// 공개 범위 변경 요청. public_permission 미지정 시 기존 값 유지.
#[derive(Debug, Deserialize)]
pub struct SetVisibilityRequest {
    pub visibility: WorkspaceVisibility,
    #[serde(default)]
    pub public_permission: Option<Permission>,
}

/// 워크스페이스 존재 검사 → 404. 모든 멤버십 엔드포인트의 공통 선행 조건.
async fn ensure_workspace_exists(
    workspaces: &WorkspaceManager,
    workspace_id: &str,
) -> Result<(), (StatusCode, String)> {
    if workspaces.exists(workspace_id).await {
        Ok(())
    } else {
        Err((
            StatusCode::NOT_FOUND,
            format!("Workspace '{}' not found", workspace_id),
        ))
    }
}

/// 멤버 추가/변경 코어 (AppState 없이 테스트 가능).
/// 워크스페이스·계정 존재를 검증한 뒤 upsert한다.
pub(crate) async fn upsert_member_checked(
    memberships: &MembershipManager,
    users: &UserManager,
    workspaces: &WorkspaceManager,
    workspace_id: &str,
    user_id: &str,
    role: Permission,
) -> Result<WorkspaceMembers, (StatusCode, String)> {
    ensure_workspace_exists(workspaces, workspace_id).await?;

    if users.get(user_id).await.is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            format!("User '{}' not found", user_id),
        ));
    }

    memberships
        .upsert_member(workspace_id, user_id, role)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// 멤버 제거 코어 (AppState 없이 테스트 가능).
/// 계정 존재는 요구하지 않는다 — 삭제된 계정의 잔존 멤버십도 제거할 수 있어야 한다.
pub(crate) async fn remove_member_checked(
    memberships: &MembershipManager,
    workspaces: &WorkspaceManager,
    workspace_id: &str,
    user_id: &str,
) -> Result<(), (StatusCode, String)> {
    ensure_workspace_exists(workspaces, workspace_id).await?;

    memberships
        .remove_member(workspace_id, user_id)
        .await
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("not a member") {
                (StatusCode::NOT_FOUND, msg)
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, msg)
            }
        })
}

/// 공개 범위 변경 코어 (AppState 없이 테스트 가능).
/// public_permission=admin은 저장소 레이어에서 거부된다 → 400으로 매핑.
pub(crate) async fn set_visibility_checked(
    memberships: &MembershipManager,
    workspaces: &WorkspaceManager,
    workspace_id: &str,
    req: SetVisibilityRequest,
) -> Result<WorkspaceMembers, (StatusCode, String)> {
    ensure_workspace_exists(workspaces, workspace_id).await?;

    memberships
        .set_visibility(workspace_id, req.visibility, req.public_permission)
        .await
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("cannot be 'admin'") {
                (StatusCode::BAD_REQUEST, msg)
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, msg)
            }
        })
}

/// GET /api/workspaces/:id/members — 멤버십·공개 설정 조회
/// (글로벌 admin 또는 해당 워크스페이스 role=admin)
pub async fn list_members_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<String>,
) -> Result<Json<MembersResponse>, (StatusCode, String)> {
    ensure_workspace_exists(&state.workspaces, &workspace_id).await?;
    require_workspace_admin(&ctx, &workspace_id)?;

    let record = state.memberships.get(&workspace_id).await;
    let mut members = Vec::with_capacity(record.members.len());
    for m in &record.members {
        let user = state.users.get(&m.user_id).await;
        members.push(MemberView {
            user_id: m.user_id.clone(),
            username: user.as_ref().map(|u| u.username.clone()),
            display_name: user.map(|u| u.display_name),
            role: m.role.clone(),
        });
    }

    Ok(Json(MembersResponse {
        visibility: record.visibility,
        public_permission: record.public_permission,
        members,
    }))
}

/// PUT /api/workspaces/:id/members/:user_id — 멤버 추가/역할 변경
/// (글로벌 admin 또는 해당 워크스페이스 role=admin)
pub async fn upsert_member_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace_id, user_id)): Path<(String, String)>,
    Json(req): Json<UpsertMemberRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    ensure_workspace_exists(&state.workspaces, &workspace_id).await?;
    require_workspace_admin(&ctx, &workspace_id)?;

    upsert_member_checked(
        &state.memberships,
        &state.users,
        &state.workspaces,
        &workspace_id,
        &user_id,
        req.role,
    )
    .await?;

    Ok(StatusCode::NO_CONTENT)
}

/// DELETE /api/workspaces/:id/members/:user_id — 멤버 제거
/// (글로벌 admin 또는 해당 워크스페이스 role=admin)
pub async fn remove_member_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Path((workspace_id, user_id)): Path<(String, String)>,
) -> Result<StatusCode, (StatusCode, String)> {
    ensure_workspace_exists(&state.workspaces, &workspace_id).await?;
    require_workspace_admin(&ctx, &workspace_id)?;

    remove_member_checked(&state.memberships, &state.workspaces, &workspace_id, &user_id).await?;

    Ok(StatusCode::NO_CONTENT)
}

/// PUT /api/workspaces/:id/visibility — 공개 범위 변경
/// (글로벌 admin 또는 해당 워크스페이스 role=admin)
pub async fn set_visibility_handler(
    State(state): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Path(workspace_id): Path<String>,
    Json(req): Json<SetVisibilityRequest>,
) -> Result<Json<MembersResponse>, (StatusCode, String)> {
    ensure_workspace_exists(&state.workspaces, &workspace_id).await?;
    require_workspace_admin(&ctx, &workspace_id)?;

    let record =
        set_visibility_checked(&state.memberships, &state.workspaces, &workspace_id, req).await?;

    Ok(Json(MembersResponse {
        visibility: record.visibility,
        public_permission: record.public_permission,
        members: record
            .members
            .into_iter()
            .map(|m| MemberView {
                user_id: m.user_id,
                username: None,
                display_name: None,
                role: m.role,
            })
            .collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{WorkspaceConfig, WorkspaceTemplate};
    use tempfile::TempDir;

    struct Fixture {
        _tmp: TempDir,
        users: UserManager,
        memberships: MembershipManager,
        workspaces: WorkspaceManager,
    }

    async fn setup() -> Fixture {
        let tmp = TempDir::new().unwrap();
        let users = UserManager::new(tmp.path().to_str().unwrap()).await.unwrap();
        let memberships = MembershipManager::new(tmp.path());
        let workspaces = WorkspaceManager::new(tmp.path()).await.unwrap();
        workspaces
            .create(WorkspaceConfig::new(
                "ws".to_string(),
                "WS".to_string(),
                WorkspaceTemplate::Personal,
            ))
            .await
            .unwrap();
        Fixture { _tmp: tmp, users, memberships, workspaces }
    }

    #[tokio::test]
    async fn test_upsert_member_checked_happy_and_missing_targets() {
        let f = setup().await;
        let user = f
            .users
            .create_user("alice", "password123", "A".to_string(), false)
            .await
            .unwrap();

        // 정상: 멤버 등록
        let record = upsert_member_checked(
            &f.memberships, &f.users, &f.workspaces, "ws", &user.user_id, Permission::ReadWrite,
        )
        .await
        .unwrap();
        assert_eq!(record.permission_of(&user.user_id), Some(Permission::ReadWrite));

        // 없는 워크스페이스 → 404
        let err = upsert_member_checked(
            &f.memberships, &f.users, &f.workspaces, "ghost", &user.user_id, Permission::ReadOnly,
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);

        // 없는 계정 → 404 (오타 user_id로 유령 멤버가 생기지 않게)
        let err = upsert_member_checked(
            &f.memberships, &f.users, &f.workspaces, "ws", "user_ghost", Permission::ReadOnly,
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_remove_member_checked_allows_deleted_user_cleanup() {
        let f = setup().await;
        let user = f
            .users
            .create_user("bob", "password123", "B".to_string(), false)
            .await
            .unwrap();
        upsert_member_checked(
            &f.memberships, &f.users, &f.workspaces, "ws", &user.user_id, Permission::ReadOnly,
        )
        .await
        .unwrap();

        // 계정을 삭제해도 잔존 멤버십은 제거 가능해야 한다 (계정 존재 비요구)
        f.users.delete_user(&user.user_id).await.unwrap();
        remove_member_checked(&f.memberships, &f.workspaces, "ws", &user.user_id)
            .await
            .unwrap();

        // 비멤버 재제거 → 404
        let err = remove_member_checked(&f.memberships, &f.workspaces, "ws", &user.user_id)
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_set_visibility_checked_admin_permission_rejected_400() {
        let f = setup().await;
        let err = set_visibility_checked(
            &f.memberships,
            &f.workspaces,
            "ws",
            SetVisibilityRequest {
                visibility: WorkspaceVisibility::Public,
                public_permission: Some(Permission::Admin),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST, "public admin은 400으로 거부");

        // 정상 전환은 반영된다
        let record = set_visibility_checked(
            &f.memberships,
            &f.workspaces,
            "ws",
            SetVisibilityRequest {
                visibility: WorkspaceVisibility::Public,
                public_permission: Some(Permission::ReadWrite),
            },
        )
        .await
        .unwrap();
        assert_eq!(record.visibility, WorkspaceVisibility::Public);
        assert_eq!(record.public_permission, Permission::ReadWrite);
    }

    #[tokio::test]
    async fn test_set_visibility_checked_missing_workspace_404() {
        let f = setup().await;
        let err = set_visibility_checked(
            &f.memberships,
            &f.workspaces,
            "ghost",
            SetVisibilityRequest {
                visibility: WorkspaceVisibility::Public,
                public_permission: None,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }
}
