use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use tokio::fs;
use tokio::sync::RwLock;

use crate::auth::Permission;

// ──────────────────────────────────────────────────────────────
// WorkspaceMembers — 워크스페이스별 멤버십 + 공개 설정
// ──────────────────────────────────────────────────────────────

/// 워크스페이스 공개 범위
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceVisibility {
    /// 멤버만 접근 가능 (기본값 — 파일 없는 기존 워크스페이스 하위호환)
    #[default]
    Private,
    /// 모든 로그인 계정이 `public_permission` 권한으로 접근 가능
    Public,
}

/// 워크스페이스 멤버 항목. role은 기존 Permission enum을 재사용한다
/// (admin = 해당 워크스페이스의 owner급 — 멤버 관리·공개 토글 가능).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceMember {
    pub user_id: String,
    pub role: Permission,
}

/// 워크스페이스 멤버십 레코드.
/// 파일 저장: `data/workspaces/{id}/members.json`
/// 파일이 없으면 기본값(private + 빈 멤버) — 기존 워크스페이스 하위호환.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceMembers {
    #[serde(default)]
    pub visibility: WorkspaceVisibility,
    /// public일 때 비멤버 로그인 계정에게 부여되는 권한.
    /// read_only | read_write만 유효 (admin은 set_visibility에서 거부).
    #[serde(default = "default_public_permission")]
    pub public_permission: Permission,
    #[serde(default)]
    pub members: Vec<WorkspaceMember>,
}

fn default_public_permission() -> Permission {
    Permission::ReadOnly
}

impl Default for WorkspaceMembers {
    fn default() -> Self {
        Self {
            visibility: WorkspaceVisibility::Private,
            public_permission: default_public_permission(),
            members: Vec::new(),
        }
    }
}

impl WorkspaceMembers {
    /// 특정 계정의 이 워크스페이스에 대한 유효 권한을 판정한다 (순수 함수).
    ///
    /// - 멤버면 자기 role
    /// - 비멤버 + public이면 public_permission
    /// - 비멤버 + private이면 None (접근 없음, fail-closed)
    pub fn permission_of(&self, user_id: &str) -> Option<Permission> {
        if let Some(member) = self.members.iter().find(|m| m.user_id == user_id) {
            return Some(member.role.clone());
        }
        match self.visibility {
            WorkspaceVisibility::Public => Some(self.public_permission.clone()),
            WorkspaceVisibility::Private => None,
        }
    }
}

// ──────────────────────────────────────────────────────────────
// MembershipManager — 멤버십 CRUD + 파일 시스템 영속화
// ──────────────────────────────────────────────────────────────

/// 워크스페이스 멤버십 관리자.
/// `data/workspaces/{id}/members.json`을 lazy 로드해 메모리 캐시를 유지한다.
/// 파일이 없는 워크스페이스는 기본값(private + 빈 멤버)으로 동작한다.
pub struct MembershipManager {
    data_dir: PathBuf,
    cache: RwLock<HashMap<String, WorkspaceMembers>>,
    /// 변경 직렬화 락. read-modify-write 전 구간을 감싸 동시 변경 유실과
    /// temp 파일 경합을 막는다 (워크스페이스 수가 적어 전역 락으로 충분).
    save_lock: tokio::sync::Mutex<()>,
}

impl MembershipManager {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            cache: RwLock::new(HashMap::new()),
            save_lock: tokio::sync::Mutex::new(()),
        }
    }

    fn members_path(&self, workspace_id: &str) -> PathBuf {
        self.data_dir
            .join("workspaces")
            .join(workspace_id)
            .join("members.json")
    }

    /// 워크스페이스 멤버십 레코드를 조회한다 (캐시 → 디스크 → 기본값).
    pub async fn get(&self, workspace_id: &str) -> WorkspaceMembers {
        {
            let cache = self.cache.read().await;
            if let Some(record) = cache.get(workspace_id) {
                return record.clone();
            }
        }

        let record = self.load_from_disk(workspace_id).await;

        let mut cache = self.cache.write().await;
        cache
            .entry(workspace_id.to_string())
            .or_insert_with(|| record.clone());
        record
    }

    /// 디스크에서 멤버십 파일을 로드한다. 없으면 기본값, 손상 시 백업 후 기본값.
    ///
    /// 손상 degrade는 fail-closed다: 기본값(private + 빈 멤버)으로 떨어지므로
    /// 접근이 넓어지는 방향의 오류는 발생하지 않는다.
    async fn load_from_disk(&self, workspace_id: &str) -> WorkspaceMembers {
        let path = self.members_path(workspace_id);
        if !path.exists() {
            return WorkspaceMembers::default();
        }

        match fs::read_to_string(&path).await {
            Ok(content) => match serde_json::from_str::<WorkspaceMembers>(&content) {
                Ok(record) => record,
                Err(e) => {
                    let backup = path.with_file_name("members.json.corrupt");
                    if let Err(re) = fs::rename(&path, &backup).await {
                        tracing::error!("손상된 members.json 백업 실패 ({}): {}", workspace_id, re);
                    }
                    tracing::error!(
                        "members.json 파싱 실패({}, workspace={}). 손상 파일을 {:?}로 백업하고 \
                         기본값(private·멤버 없음)으로 동작합니다. 멤버십을 재설정하세요.",
                        e,
                        workspace_id,
                        backup
                    );
                    WorkspaceMembers::default()
                }
            },
            Err(e) => {
                tracing::error!("members.json 읽기 실패 ({}): {}", workspace_id, e);
                WorkspaceMembers::default()
            }
        }
    }

    /// 멤버를 추가하거나 role을 변경한다 (초대 = role 지정 추가, 수락 플로우 없음).
    pub async fn upsert_member(
        &self,
        workspace_id: &str,
        user_id: &str,
        role: Permission,
    ) -> Result<WorkspaceMembers> {
        let _guard = self.save_lock.lock().await;

        let mut record = self.get(workspace_id).await;
        match record.members.iter_mut().find(|m| m.user_id == user_id) {
            Some(member) => member.role = role,
            None => record.members.push(WorkspaceMember {
                user_id: user_id.to_string(),
                role,
            }),
        }

        self.persist(workspace_id, &record).await?;
        tracing::info!("Upserted member {} in workspace {}", user_id, workspace_id);
        Ok(record)
    }

    /// 멤버를 제거한다. 멤버가 아니면 에러.
    pub async fn remove_member(&self, workspace_id: &str, user_id: &str) -> Result<()> {
        let _guard = self.save_lock.lock().await;

        let mut record = self.get(workspace_id).await;
        let before = record.members.len();
        record.members.retain(|m| m.user_id != user_id);
        if record.members.len() == before {
            return Err(anyhow!(
                "User '{}' is not a member of workspace '{}'",
                user_id,
                workspace_id
            ));
        }

        self.persist(workspace_id, &record).await?;
        tracing::info!("Removed member {} from workspace {}", user_id, workspace_id);
        Ok(())
    }

    /// 공개 범위를 변경한다. public_permission에 admin은 허용하지 않는다
    /// (비멤버 전원에게 owner급을 주는 설정은 오설정으로 간주).
    pub async fn set_visibility(
        &self,
        workspace_id: &str,
        visibility: WorkspaceVisibility,
        public_permission: Option<Permission>,
    ) -> Result<WorkspaceMembers> {
        if let Some(Permission::Admin) = public_permission {
            return Err(anyhow!("public_permission cannot be 'admin'"));
        }

        let _guard = self.save_lock.lock().await;

        let mut record = self.get(workspace_id).await;
        record.visibility = visibility;
        if let Some(perm) = public_permission {
            record.public_permission = perm;
        }

        self.persist(workspace_id, &record).await?;
        tracing::info!("Set visibility of workspace {}", workspace_id);
        Ok(record)
    }

    /// 워크스페이스 삭제 시 캐시를 무효화한다 (파일은 디렉토리와 함께 제거됨).
    pub async fn forget_workspace(&self, workspace_id: &str) {
        let mut cache = self.cache.write().await;
        cache.remove(workspace_id);
    }

    /// 계정 삭제 연쇄 조치: 모든 워크스페이스의 멤버십에서 해당 계정을 제거한다.
    /// (유령 멤버 잔존 방지 — best-effort, 실패한 워크스페이스는 로그로 남긴다)
    pub async fn remove_user_everywhere(&self, user_id: &str, workspace_ids: &[String]) {
        for ws in workspace_ids {
            let record = self.get(ws).await;
            if record.members.iter().any(|m| m.user_id == user_id) {
                if let Err(e) = self.remove_member(ws, user_id).await {
                    tracing::warn!(
                        "계정 삭제 연쇄 정리 실패 (workspace={}, user={}): {}",
                        ws,
                        user_id,
                        e
                    );
                }
            }
        }
    }

    /// 계정의 접근 가능 워크스페이스 → 유효 권한 맵을 계산한다.
    /// (멤버인 ws 전체 ∪ visibility=public인 ws 전체)
    ///
    /// BTreeMap을 반환해 순회 순서를 결정적으로 유지한다
    /// (default_workspace 선택 등 하위 판정의 예측 가능성).
    pub async fn access_map_for_user(
        &self,
        user_id: &str,
        workspace_ids: &[String],
    ) -> BTreeMap<String, Permission> {
        let mut map = BTreeMap::new();
        for ws in workspace_ids {
            let record = self.get(ws).await;
            if let Some(perm) = record.permission_of(user_id) {
                map.insert(ws.clone(), perm);
            }
        }
        map
    }

    /// 멤버십 레코드를 디스크에 원자적으로 영속화하고 캐시를 갱신한다.
    /// (호출측이 save_lock을 잡고 있어야 한다 — RMW 직렬화)
    async fn persist(&self, workspace_id: &str, record: &WorkspaceMembers) -> Result<()> {
        let path = self.members_path(workspace_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }

        let content = serde_json::to_string_pretty(record)?;
        let tmp_path = path.with_file_name("members.json.tmp");

        fs::write(&tmp_path, content)
            .await
            .context("Failed to write members.json.tmp")?;
        fs::rename(&tmp_path, &path)
            .await
            .context("Failed to persist members.json")?;

        let mut cache = self.cache.write().await;
        cache.insert(workspace_id.to_string(), record.clone());
        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, MembershipManager) {
        let tmp = TempDir::new().unwrap();
        let manager = MembershipManager::new(tmp.path());
        (tmp, manager)
    }

    // ──── 기본값 / 하위호환 ────

    #[tokio::test]
    async fn test_missing_file_defaults_to_private_empty() {
        // members.json이 없는 기존 워크스페이스는 private + 빈 멤버로 동작해야 한다
        // (하위호환 — 마이그레이션 없이 기존 접근 모델 유지).
        let (_tmp, manager) = setup();
        let record = manager.get("legacy-ws").await;
        assert_eq!(record.visibility, WorkspaceVisibility::Private);
        assert!(record.members.is_empty());
        assert_eq!(record.permission_of("user_any"), None, "fail-closed");
    }

    #[test]
    fn test_partial_json_fills_defaults() {
        // 필드 일부만 있는 파일도 serde default로 로드된다
        let record: WorkspaceMembers = serde_json::from_str(r#"{"visibility": "public"}"#).unwrap();
        assert_eq!(record.visibility, WorkspaceVisibility::Public);
        assert_eq!(record.public_permission, Permission::ReadOnly);
        assert!(record.members.is_empty());

        let record: WorkspaceMembers = serde_json::from_str("{}").unwrap();
        assert_eq!(record.visibility, WorkspaceVisibility::Private);
    }

    #[test]
    fn test_spec_json_shape_roundtrip() {
        // 명세된 파일 포맷이 그대로 파싱되는지 고정하는 계약 테스트
        let json = r#"{
            "visibility": "public",
            "public_permission": "read_write",
            "members": [{"user_id": "user_abc123", "role": "admin"}]
        }"#;
        let record: WorkspaceMembers = serde_json::from_str(json).unwrap();
        assert_eq!(record.visibility, WorkspaceVisibility::Public);
        assert_eq!(record.public_permission, Permission::ReadWrite);
        assert_eq!(record.members.len(), 1);
        assert_eq!(record.members[0].role, Permission::Admin);

        let serialized = serde_json::to_string(&record).unwrap();
        assert!(serialized.contains("\"public\""));
        assert!(serialized.contains("\"read_write\""));
    }

    // ──── permission_of (순수 판정) ────

    #[test]
    fn test_permission_of_member_role() {
        let record = WorkspaceMembers {
            visibility: WorkspaceVisibility::Private,
            public_permission: Permission::ReadOnly,
            members: vec![
                WorkspaceMember { user_id: "u1".into(), role: Permission::ReadOnly },
                WorkspaceMember { user_id: "u2".into(), role: Permission::Admin },
            ],
        };
        assert_eq!(record.permission_of("u1"), Some(Permission::ReadOnly));
        assert_eq!(record.permission_of("u2"), Some(Permission::Admin));
        assert_eq!(record.permission_of("stranger"), None);
    }

    #[test]
    fn test_permission_of_public_grants_public_permission() {
        let record = WorkspaceMembers {
            visibility: WorkspaceVisibility::Public,
            public_permission: Permission::ReadWrite,
            members: vec![WorkspaceMember { user_id: "u1".into(), role: Permission::ReadOnly }],
        };
        // 비멤버는 public_permission
        assert_eq!(record.permission_of("stranger"), Some(Permission::ReadWrite));
        // 멤버는 public_permission보다 자기 role이 우선한다 (명시 role 존중)
        assert_eq!(record.permission_of("u1"), Some(Permission::ReadOnly));
    }

    // ──── upsert / remove ────

    #[tokio::test]
    async fn test_upsert_member_add_and_update() {
        let (_tmp, manager) = setup();

        manager.upsert_member("ws", "u1", Permission::ReadOnly).await.unwrap();
        let record = manager.get("ws").await;
        assert_eq!(record.permission_of("u1"), Some(Permission::ReadOnly));

        // 같은 user_id 재호출은 role 변경 (중복 항목 금지)
        manager.upsert_member("ws", "u1", Permission::Admin).await.unwrap();
        let record = manager.get("ws").await;
        assert_eq!(record.members.len(), 1);
        assert_eq!(record.permission_of("u1"), Some(Permission::Admin));
    }

    #[tokio::test]
    async fn test_remove_member() {
        let (_tmp, manager) = setup();
        manager.upsert_member("ws", "u1", Permission::ReadWrite).await.unwrap();

        manager.remove_member("ws", "u1").await.unwrap();
        assert_eq!(manager.get("ws").await.permission_of("u1"), None);

        // 비멤버 제거는 에러
        let err = manager.remove_member("ws", "u1").await.unwrap_err();
        assert!(err.to_string().contains("not a member"));
    }

    // ──── visibility ────

    #[tokio::test]
    async fn test_set_visibility_public_and_back() {
        let (_tmp, manager) = setup();

        manager
            .set_visibility("ws", WorkspaceVisibility::Public, Some(Permission::ReadWrite))
            .await
            .unwrap();
        let record = manager.get("ws").await;
        assert_eq!(record.permission_of("anyone"), Some(Permission::ReadWrite));

        // private으로 되돌리면 비멤버 접근이 즉시 차단된다
        manager
            .set_visibility("ws", WorkspaceVisibility::Private, None)
            .await
            .unwrap();
        let record = manager.get("ws").await;
        assert_eq!(record.permission_of("anyone"), None);
    }

    #[tokio::test]
    async fn test_set_visibility_rejects_admin_public_permission() {
        let (_tmp, manager) = setup();
        let err = manager
            .set_visibility("ws", WorkspaceVisibility::Public, Some(Permission::Admin))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cannot be 'admin'"));
    }

    // ──── access_map_for_user ────

    #[tokio::test]
    async fn test_access_map_membership_union_public() {
        let (_tmp, manager) = setup();
        // ws-a: 멤버 (read_write)
        manager.upsert_member("ws-a", "u1", Permission::ReadWrite).await.unwrap();
        // ws-b: public (read_only)
        manager
            .set_visibility("ws-b", WorkspaceVisibility::Public, Some(Permission::ReadOnly))
            .await
            .unwrap();
        // ws-c: private, 비멤버 → 접근 없음

        let all = vec!["ws-a".to_string(), "ws-b".to_string(), "ws-c".to_string()];
        let map = manager.access_map_for_user("u1", &all).await;

        assert_eq!(map.get("ws-a"), Some(&Permission::ReadWrite));
        assert_eq!(map.get("ws-b"), Some(&Permission::ReadOnly));
        assert!(!map.contains_key("ws-c"), "private 비멤버는 맵에 없어야 한다");
    }

    #[tokio::test]
    async fn test_access_map_member_role_overrides_public() {
        let (_tmp, manager) = setup();
        // public read_write지만 멤버로는 read_only — 명시 role이 우선
        manager
            .set_visibility("ws", WorkspaceVisibility::Public, Some(Permission::ReadWrite))
            .await
            .unwrap();
        manager.upsert_member("ws", "u1", Permission::ReadOnly).await.unwrap();

        let map = manager.access_map_for_user("u1", &["ws".to_string()]).await;
        assert_eq!(map.get("ws"), Some(&Permission::ReadOnly));
    }

    // ──── 연쇄 정리 / 캐시 ────

    #[tokio::test]
    async fn test_remove_user_everywhere() {
        let (_tmp, manager) = setup();
        manager.upsert_member("ws-a", "u1", Permission::ReadWrite).await.unwrap();
        manager.upsert_member("ws-b", "u1", Permission::Admin).await.unwrap();
        manager.upsert_member("ws-b", "u2", Permission::ReadOnly).await.unwrap();

        let all = vec!["ws-a".to_string(), "ws-b".to_string()];
        manager.remove_user_everywhere("u1", &all).await;

        assert_eq!(manager.get("ws-a").await.permission_of("u1"), None);
        assert_eq!(manager.get("ws-b").await.permission_of("u1"), None);
        // 다른 멤버는 영향 없음
        assert_eq!(manager.get("ws-b").await.permission_of("u2"), Some(Permission::ReadOnly));
    }

    #[tokio::test]
    async fn test_forget_workspace_invalidates_cache() {
        let (tmp, manager) = setup();
        manager.upsert_member("ws", "u1", Permission::ReadWrite).await.unwrap();

        // 워크스페이스 디렉토리 삭제 시뮬레이션 (WorkspaceManager::delete와 동일 효과)
        fs::remove_dir_all(tmp.path().join("workspaces/ws")).await.unwrap();
        manager.forget_workspace("ws").await;

        // 캐시가 무효화되어 기본값(접근 없음)으로 돌아가야 한다
        let record = manager.get("ws").await;
        assert_eq!(record.permission_of("u1"), None, "삭제된 ws의 멤버십이 캐시에 남으면 안 된다");
    }

    // ──── 영속화 ────

    #[tokio::test]
    async fn test_persists_to_disk_and_reloads() {
        let (tmp, manager) = setup();
        manager.upsert_member("ws", "u1", Permission::Admin).await.unwrap();
        manager
            .set_visibility("ws", WorkspaceVisibility::Public, Some(Permission::ReadOnly))
            .await
            .unwrap();

        // 파일 위치 계약: data/workspaces/{id}/members.json
        assert!(tmp.path().join("workspaces/ws/members.json").exists());

        // 새 매니저로 재로드 (재시작 시뮬레이션)
        let manager2 = MembershipManager::new(tmp.path());
        let record = manager2.get("ws").await;
        assert_eq!(record.visibility, WorkspaceVisibility::Public);
        assert_eq!(record.permission_of("u1"), Some(Permission::Admin));
    }

    #[tokio::test]
    async fn test_corrupt_file_degrades_to_default_fail_closed() {
        let (tmp, manager) = setup();
        let ws_dir = tmp.path().join("workspaces/ws");
        fs::create_dir_all(&ws_dir).await.unwrap();
        fs::write(ws_dir.join("members.json"), "{ broken !!").await.unwrap();

        // 손상 파일은 접근이 "넓어지는" 방향이 아니라 기본값(private)으로 떨어져야 한다
        let record = manager.get("ws").await;
        assert_eq!(record.visibility, WorkspaceVisibility::Private);
        assert_eq!(record.permission_of("u1"), None);
        assert!(ws_dir.join("members.json.corrupt").exists());
    }

    #[tokio::test]
    async fn test_save_is_atomic_no_temp_left() {
        let (tmp, manager) = setup();
        manager.upsert_member("ws", "u1", Permission::ReadOnly).await.unwrap();
        assert!(!tmp.path().join("workspaces/ws/members.json.tmp").exists());
    }
}
