use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use tokio::fs;
use tokio::sync::RwLock;
use uuid::Uuid;

// ──────────────────────────────────────────────────────────────
// Permission
// ──────────────────────────────────────────────────────────────

/// API Key 접근 권한 수준
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    ReadOnly,
    ReadWrite,
    Admin,
}

impl Permission {
    pub fn can_write(&self) -> bool {
        matches!(self, Permission::ReadWrite | Permission::Admin)
    }

    pub fn is_admin(&self) -> bool {
        matches!(self, Permission::Admin)
    }
}

// ──────────────────────────────────────────────────────────────
// ApiKey
// ──────────────────────────────────────────────────────────────

/// 저장되는 API Key 구조체.
/// 평문 키는 저장하지 않고, SHA-256 해시만 보관한다.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    /// 키 식별자 (prefix `maia_sk_`)
    pub key_id: String,
    /// SHA-256 해시 (`sha256:` prefix)
    pub hashed_key: String,
    /// 사람이 읽을 수 있는 라벨 ("내 맥북", "동료 B" 등)
    pub label: String,
    /// 접근 가능한 워크스페이스 ID 목록
    pub workspaces: Vec<String>,
    /// 권한 수준
    pub permissions: Permission,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    /// 만료일 (None이면 무기한)
    pub expires_at: Option<DateTime<Utc>>,
}

impl ApiKey {
    /// 키가 만료되었는지 확인
    pub fn is_expired(&self) -> bool {
        self.expires_at
            .map_or(false, |exp| Utc::now() > exp)
    }

    /// 특정 워크스페이스에 접근 가능한지 확인
    pub fn has_workspace_access(&self, workspace_id: &str) -> bool {
        self.workspaces.iter().any(|w| w == workspace_id)
    }
}

// ──────────────────────────────────────────────────────────────
// AuthContext — 미들웨어가 요청에 주입하는 인증 컨텍스트
// ──────────────────────────────────────────────────────────────

/// 인증된 요청의 컨텍스트. 미들웨어가 검증 후 Request Extensions에 삽입한다.
#[derive(Debug, Clone)]
pub struct AuthContext {
    /// 키 식별자 ("master" 또는 "dev" 또는 실제 key_id)
    pub key_id: String,
    /// 권한 수준
    pub permissions: Permission,
    /// 접근 가능한 워크스페이스 목록. 비어있으면 전체 접근 (마스터키/개발모드).
    pub workspaces: Vec<String>,
    /// 마스터키 또는 개발모드 여부
    pub is_master: bool,
}

impl AuthContext {
    /// 마스터키(MAIA_API_KEY) 인증 시 생성
    pub fn master() -> Self {
        Self {
            key_id: "master".to_string(),
            permissions: Permission::Admin,
            workspaces: vec![],
            is_master: true,
        }
    }

    /// 개발 모드 (인증 미설정) 시 생성
    pub fn dev_mode() -> Self {
        Self {
            key_id: "dev".to_string(),
            permissions: Permission::Admin,
            workspaces: vec![],
            is_master: true,
        }
    }

    /// API Key로부터 AuthContext 생성
    pub fn from_api_key(key: &ApiKey) -> Self {
        Self {
            key_id: key.key_id.clone(),
            permissions: key.permissions.clone(),
            workspaces: key.workspaces.clone(),
            is_master: false,
        }
    }

    /// 특정 워크스페이스에 접근 가능한지 확인.
    /// 마스터키/개발모드는 항상 true, API Key는 workspaces 목록 기반.
    pub fn can_access_workspace(&self, workspace_id: &str) -> bool {
        self.is_master || self.workspaces.is_empty() || self.workspaces.iter().any(|w| w == workspace_id)
    }

    pub fn can_write(&self) -> bool {
        self.permissions.can_write()
    }

    pub fn is_admin(&self) -> bool {
        self.permissions.is_admin()
    }
}

// ──────────────────────────────────────────────────────────────
// Crypto helpers
// ──────────────────────────────────────────────────────────────

/// 평문 키를 SHA-256 해시로 변환
pub fn hash_key(raw_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw_key.as_bytes());
    let result = hasher.finalize();
    let hex: String = result.iter().map(|b| format!("{:02x}", b)).collect();
    format!("sha256:{}", hex)
}

/// 고유 key_id 생성 (`maia_sk_` + 16자 hex)
pub fn generate_key_id() -> String {
    let uuid = Uuid::new_v4().to_string().replace('-', "");
    format!("maia_sk_{}", &uuid[..16])
}

/// 보안 랜덤 평문 키 생성 (`maia_` + 32자 hex)
pub fn generate_raw_key() -> String {
    let uuid = Uuid::new_v4().to_string().replace('-', "");
    format!("maia_{}", uuid)
}

// ──────────────────────────────────────────────────────────────
// ApiKeyManager — 키 CRUD + 파일 시스템 영속화
// ──────────────────────────────────────────────────────────────

/// API Key 관리자. `data/api_keys.json`에 키를 영속화하고 메모리 캐시를 유지한다.
pub struct ApiKeyManager {
    keys_path: PathBuf,
    keys: RwLock<Vec<ApiKey>>,
}

impl ApiKeyManager {
    /// `data_dir/api_keys.json`에서 키 목록을 로드한다.
    /// 파일이 없으면 빈 목록으로 시작한다.
    pub async fn new(data_dir: &str) -> Result<Self> {
        let keys_path = PathBuf::from(data_dir).join("api_keys.json");

        let keys = if keys_path.exists() {
            let content = fs::read_to_string(&keys_path)
                .await
                .context("Failed to read api_keys.json")?;
            serde_json::from_str(&content)
                .context("Failed to parse api_keys.json")?
        } else {
            Vec::new()
        };

        Ok(Self {
            keys_path,
            keys: RwLock::new(keys),
        })
    }

    /// 새 API Key를 생성한다. (ApiKey, 평문 키)를 반환한다.
    /// 평문 키는 이 시점에서만 확인 가능하며, 이후에는 해시만 저장된다.
    pub async fn create_key(
        &self,
        label: String,
        workspaces: Vec<String>,
        permissions: Permission,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<(ApiKey, String)> {
        let key_id = generate_key_id();
        let raw_key = generate_raw_key();
        let hashed = hash_key(&raw_key);

        let api_key = ApiKey {
            key_id,
            hashed_key: hashed,
            label,
            workspaces,
            permissions,
            created_at: Utc::now(),
            last_used_at: None,
            expires_at,
        };

        {
            let mut keys = self.keys.write().await;
            keys.push(api_key.clone());
        }

        self.save().await?;

        tracing::info!("Created API key: {}", api_key.key_id);
        Ok((api_key, raw_key))
    }

    /// 저장된 모든 키 목록을 반환한다.
    pub async fn list_keys(&self) -> Vec<ApiKey> {
        self.keys.read().await.clone()
    }

    /// 키를 폐기(삭제)한다.
    pub async fn revoke_key(&self, key_id: &str) -> Result<()> {
        {
            let mut keys = self.keys.write().await;
            let idx = keys.iter().position(|k| k.key_id == key_id)
                .ok_or_else(|| anyhow!("API key not found: {}", key_id))?;
            keys.remove(idx);
        }

        self.save().await?;

        tracing::info!("Revoked API key: {}", key_id);
        Ok(())
    }

    /// 해시로 키를 검색한다. 인증 미들웨어에서 사용.
    pub async fn find_by_hash(&self, hashed_key: &str) -> Option<ApiKey> {
        let keys = self.keys.read().await;
        keys.iter().find(|k| k.hashed_key == hashed_key).cloned()
    }

    /// 등록된 키가 있는지 확인한다.
    pub async fn has_keys(&self) -> bool {
        !self.keys.read().await.is_empty()
    }

    /// `last_used_at`을 현재 시각으로 갱신한다.
    /// 미들웨어에서 `tokio::spawn`으로 비동기 호출하여 응답 지연을 방지한다.
    pub async fn update_last_used(&self, key_id: &str) -> Result<()> {
        {
            let mut keys = self.keys.write().await;
            if let Some(key) = keys.iter_mut().find(|k| k.key_id == key_id) {
                key.last_used_at = Some(Utc::now());
            }
        }

        self.save().await
    }

    /// 현재 키 목록을 디스크에 영속화한다.
    async fn save(&self) -> Result<()> {
        let keys = self.keys.read().await;
        let content = serde_json::to_string_pretty(&*keys)?;
        drop(keys);

        if let Some(parent) = self.keys_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        fs::write(&self.keys_path, content)
            .await
            .context("Failed to write api_keys.json")?;

        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use tempfile::TempDir;

    // ──── hash_key ────

    #[test]
    fn test_hash_key_deterministic() {
        let hash1 = hash_key("test-key-123");
        let hash2 = hash_key("test-key-123");
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_hash_key_prefix() {
        let hash = hash_key("any-key");
        assert!(hash.starts_with("sha256:"));
    }

    #[test]
    fn test_hash_key_length() {
        let hash = hash_key("any-key");
        // "sha256:" (7) + 64 hex chars = 71
        assert_eq!(hash.len(), 71);
    }

    #[test]
    fn test_hash_key_different_inputs() {
        let hash1 = hash_key("key-a");
        let hash2 = hash_key("key-b");
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_hash_key_empty_input() {
        let hash = hash_key("");
        assert!(hash.starts_with("sha256:"));
        assert_eq!(hash.len(), 71);
    }

    #[test]
    fn test_hash_key_verification_correct() {
        let raw = "my-secret-key-12345";
        let hashed = hash_key(raw);
        let re_hashed = hash_key(raw);
        assert_eq!(hashed, re_hashed, "같은 평문은 같은 해시를 생성해야 한다");
    }

    #[test]
    fn test_hash_key_verification_wrong() {
        let hashed = hash_key("correct-key");
        let wrong_hash = hash_key("wrong-key");
        assert_ne!(hashed, wrong_hash, "다른 평문은 다른 해시를 생성해야 한다");
    }

    // ──── generate_key_id ────

    #[test]
    fn test_generate_key_id_prefix() {
        let id = generate_key_id();
        assert!(id.starts_with("maia_sk_"));
    }

    #[test]
    fn test_generate_key_id_unique() {
        let id1 = generate_key_id();
        let id2 = generate_key_id();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_generate_key_id_length() {
        let id = generate_key_id();
        // "maia_sk_" (8) + 16 hex chars = 24
        assert_eq!(id.len(), 24);
    }

    // ──── generate_raw_key ────

    #[test]
    fn test_generate_raw_key_prefix() {
        let key = generate_raw_key();
        assert!(key.starts_with("maia_"));
    }

    #[test]
    fn test_generate_raw_key_unique() {
        let key1 = generate_raw_key();
        let key2 = generate_raw_key();
        assert_ne!(key1, key2);
    }

    #[test]
    fn test_key_creation_returns_valid_prefix() {
        let key_id = generate_key_id();
        let raw_key = generate_raw_key();
        assert!(key_id.starts_with("maia_sk_"), "key_id는 maia_sk_ prefix를 가져야 한다");
        assert!(raw_key.starts_with("maia_"), "raw_key는 maia_ prefix를 가져야 한다");
    }

    // ──── Permission ────

    #[test]
    fn test_permission_can_write() {
        assert!(!Permission::ReadOnly.can_write());
        assert!(Permission::ReadWrite.can_write());
        assert!(Permission::Admin.can_write());
    }

    #[test]
    fn test_permission_is_admin() {
        assert!(!Permission::ReadOnly.is_admin());
        assert!(!Permission::ReadWrite.is_admin());
        assert!(Permission::Admin.is_admin());
    }

    #[test]
    fn test_permission_serialize() {
        let json = serde_json::to_string(&Permission::ReadOnly).unwrap();
        assert_eq!(json, "\"read_only\"");

        let json = serde_json::to_string(&Permission::ReadWrite).unwrap();
        assert_eq!(json, "\"read_write\"");

        let json = serde_json::to_string(&Permission::Admin).unwrap();
        assert_eq!(json, "\"admin\"");
    }

    #[test]
    fn test_permission_deserialize() {
        let p: Permission = serde_json::from_str("\"read_only\"").unwrap();
        assert_eq!(p, Permission::ReadOnly);

        let p: Permission = serde_json::from_str("\"read_write\"").unwrap();
        assert_eq!(p, Permission::ReadWrite);

        let p: Permission = serde_json::from_str("\"admin\"").unwrap();
        assert_eq!(p, Permission::Admin);
    }

    // ──── ApiKey ────

    fn make_test_key() -> ApiKey {
        ApiKey {
            key_id: "maia_sk_test1234abcd".to_string(),
            hashed_key: hash_key("raw-key"),
            label: "Test Key".to_string(),
            workspaces: vec!["default".to_string(), "work".to_string()],
            permissions: Permission::ReadWrite,
            created_at: Utc::now(),
            last_used_at: None,
            expires_at: None,
        }
    }

    #[test]
    fn test_api_key_is_expired_no_expiry() {
        let key = make_test_key();
        assert!(!key.is_expired());
    }

    #[test]
    fn test_api_key_is_expired_future() {
        let mut key = make_test_key();
        key.expires_at = Some(Utc::now() + Duration::days(30));
        assert!(!key.is_expired());
    }

    #[test]
    fn test_api_key_is_expired_past() {
        let mut key = make_test_key();
        key.expires_at = Some(Utc::now() - Duration::seconds(1));
        assert!(key.is_expired());
    }

    #[test]
    fn test_api_key_has_workspace_access() {
        let key = make_test_key();
        assert!(key.has_workspace_access("default"));
        assert!(key.has_workspace_access("work"));
        assert!(!key.has_workspace_access("personal"));
        assert!(!key.has_workspace_access(""));
    }

    #[test]
    fn test_api_key_serialize_roundtrip() {
        let key = make_test_key();
        let json = serde_json::to_string(&key).unwrap();
        let deserialized: ApiKey = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.key_id, key.key_id);
        assert_eq!(deserialized.hashed_key, key.hashed_key);
        assert_eq!(deserialized.workspaces, key.workspaces);
        assert_eq!(deserialized.permissions, key.permissions);
        assert_eq!(deserialized.label, key.label);
    }

    #[test]
    fn test_api_key_empty_workspaces() {
        let mut key = make_test_key();
        key.workspaces = vec![];
        assert!(!key.has_workspace_access("default"));
        assert!(!key.has_workspace_access("anything"));
    }

    // ──── AuthContext ────

    #[test]
    fn test_auth_context_master() {
        let ctx = AuthContext::master();
        assert!(ctx.is_master);
        assert!(ctx.is_admin());
        assert!(ctx.can_write());
        assert!(ctx.can_access_workspace("default"));
        assert!(ctx.can_access_workspace("any-workspace"));
        assert_eq!(ctx.key_id, "master");
    }

    #[test]
    fn test_auth_context_dev_mode() {
        let ctx = AuthContext::dev_mode();
        assert!(ctx.is_master);
        assert!(ctx.is_admin());
        assert!(ctx.can_access_workspace("anything"));
    }

    #[test]
    fn test_auth_context_from_api_key() {
        let key = make_test_key();
        let ctx = AuthContext::from_api_key(&key);
        assert!(!ctx.is_master);
        assert!(ctx.can_write()); // ReadWrite
        assert!(!ctx.is_admin());
        assert!(ctx.can_access_workspace("default"));
        assert!(ctx.can_access_workspace("work"));
        assert!(!ctx.can_access_workspace("personal"));
    }

    #[test]
    fn test_auth_context_read_only_cant_write() {
        let mut key = make_test_key();
        key.permissions = Permission::ReadOnly;
        let ctx = AuthContext::from_api_key(&key);
        assert!(!ctx.can_write());
        assert!(!ctx.is_admin());
    }

    #[test]
    fn test_auth_context_admin_can_all() {
        let mut key = make_test_key();
        key.permissions = Permission::Admin;
        let ctx = AuthContext::from_api_key(&key);
        assert!(ctx.can_write());
        assert!(ctx.is_admin());
    }

    #[test]
    fn test_auth_context_workspace_access_denied() {
        let mut key = make_test_key();
        key.workspaces = vec!["only-this".to_string()];
        let ctx = AuthContext::from_api_key(&key);
        assert!(ctx.can_access_workspace("only-this"));
        assert!(!ctx.can_access_workspace("default"));
        assert!(!ctx.can_access_workspace("other"));
    }

    #[test]
    fn test_master_key_grants_full_access() {
        let ctx = AuthContext::master();
        // 마스터키는 모든 워크스페이스에 admin 접근
        assert!(ctx.can_access_workspace("default"));
        assert!(ctx.can_access_workspace("work"));
        assert!(ctx.can_access_workspace("secret-workspace"));
        assert!(ctx.is_admin());
        assert!(ctx.can_write());
    }

    // ──── ApiKeyManager ────

    async fn setup_manager() -> (TempDir, ApiKeyManager) {
        let tmp = TempDir::new().unwrap();
        let manager = ApiKeyManager::new(tmp.path().to_str().unwrap()).await.unwrap();
        (tmp, manager)
    }

    #[tokio::test]
    async fn test_manager_create_key() {
        let (_tmp, manager) = setup_manager().await;

        let (key, raw) = manager.create_key(
            "Test".to_string(),
            vec!["default".to_string()],
            Permission::ReadWrite,
            None,
        ).await.unwrap();

        assert!(key.key_id.starts_with("maia_sk_"));
        assert!(raw.starts_with("maia_"));
        assert_eq!(key.label, "Test");
        assert_eq!(key.workspaces, vec!["default"]);
        assert_eq!(key.permissions, Permission::ReadWrite);
        assert!(key.last_used_at.is_none());
        assert!(key.expires_at.is_none());
    }

    #[tokio::test]
    async fn test_manager_create_key_hash_matches() {
        let (_tmp, manager) = setup_manager().await;

        let (key, raw) = manager.create_key(
            "Hash Test".to_string(),
            vec!["default".to_string()],
            Permission::ReadOnly,
            None,
        ).await.unwrap();

        // 평문 키의 해시가 저장된 해시와 일치해야 한다
        assert_eq!(hash_key(&raw), key.hashed_key);
    }

    #[tokio::test]
    async fn test_manager_list_keys_empty() {
        let (_tmp, manager) = setup_manager().await;
        let keys = manager.list_keys().await;
        assert!(keys.is_empty());
    }

    #[tokio::test]
    async fn test_manager_list_keys_multiple() {
        let (_tmp, manager) = setup_manager().await;

        manager.create_key("Key 1".to_string(), vec!["default".to_string()], Permission::ReadOnly, None).await.unwrap();
        manager.create_key("Key 2".to_string(), vec!["work".to_string()], Permission::ReadWrite, None).await.unwrap();
        manager.create_key("Key 3".to_string(), vec!["default".to_string(), "work".to_string()], Permission::Admin, None).await.unwrap();

        let keys = manager.list_keys().await;
        assert_eq!(keys.len(), 3);
        assert_eq!(keys[0].label, "Key 1");
        assert_eq!(keys[1].label, "Key 2");
        assert_eq!(keys[2].label, "Key 3");
    }

    #[tokio::test]
    async fn test_manager_find_by_hash() {
        let (_tmp, manager) = setup_manager().await;

        let (key, raw) = manager.create_key(
            "Find Me".to_string(),
            vec!["default".to_string()],
            Permission::ReadWrite,
            None,
        ).await.unwrap();

        let hashed = hash_key(&raw);
        let found = manager.find_by_hash(&hashed).await;
        assert!(found.is_some());
        assert_eq!(found.unwrap().key_id, key.key_id);
    }

    #[tokio::test]
    async fn test_manager_find_by_hash_not_found() {
        let (_tmp, manager) = setup_manager().await;
        let found = manager.find_by_hash("sha256:nonexistent").await;
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn test_manager_revoke_key() {
        let (_tmp, manager) = setup_manager().await;

        let (key, _) = manager.create_key(
            "Revoke Me".to_string(),
            vec!["default".to_string()],
            Permission::ReadWrite,
            None,
        ).await.unwrap();

        manager.revoke_key(&key.key_id).await.unwrap();

        let keys = manager.list_keys().await;
        assert!(keys.is_empty());
    }

    #[tokio::test]
    async fn test_manager_revoke_nonexistent_key() {
        let (_tmp, manager) = setup_manager().await;
        let err = manager.revoke_key("maia_sk_doesnotexist").await.unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn test_manager_revoked_key_not_findable() {
        let (_tmp, manager) = setup_manager().await;

        let (key, raw) = manager.create_key(
            "Will Revoke".to_string(),
            vec!["default".to_string()],
            Permission::ReadWrite,
            None,
        ).await.unwrap();

        let hashed = hash_key(&raw);
        assert!(manager.find_by_hash(&hashed).await.is_some());

        manager.revoke_key(&key.key_id).await.unwrap();
        assert!(manager.find_by_hash(&hashed).await.is_none(), "폐기된 키는 검색되면 안 된다");
    }

    #[tokio::test]
    async fn test_manager_has_keys() {
        let (_tmp, manager) = setup_manager().await;
        assert!(!manager.has_keys().await);

        manager.create_key("K".to_string(), vec![], Permission::ReadOnly, None).await.unwrap();
        assert!(manager.has_keys().await);
    }

    #[tokio::test]
    async fn test_manager_update_last_used() {
        let (_tmp, manager) = setup_manager().await;

        let (key, _) = manager.create_key(
            "Usage Track".to_string(),
            vec!["default".to_string()],
            Permission::ReadWrite,
            None,
        ).await.unwrap();

        assert!(key.last_used_at.is_none());

        manager.update_last_used(&key.key_id).await.unwrap();

        let keys = manager.list_keys().await;
        let updated = keys.iter().find(|k| k.key_id == key.key_id).unwrap();
        assert!(updated.last_used_at.is_some());
    }

    #[tokio::test]
    async fn test_manager_persists_to_disk() {
        let (tmp, manager) = setup_manager().await;

        manager.create_key(
            "Persist".to_string(),
            vec!["default".to_string()],
            Permission::Admin,
            None,
        ).await.unwrap();

        // 새 매니저로 디스크에서 재로드
        let manager2 = ApiKeyManager::new(tmp.path().to_str().unwrap()).await.unwrap();
        let keys = manager2.list_keys().await;
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].label, "Persist");
    }

    #[tokio::test]
    async fn test_manager_expired_key_detection() {
        let (_tmp, manager) = setup_manager().await;

        let expires = Utc::now() - Duration::seconds(10);
        let (key, raw) = manager.create_key(
            "Expired".to_string(),
            vec!["default".to_string()],
            Permission::ReadWrite,
            Some(expires),
        ).await.unwrap();

        // 키 자체는 검색 가능하지만, is_expired()가 true를 반환해야 한다
        let hashed = hash_key(&raw);
        let found = manager.find_by_hash(&hashed).await.unwrap();
        assert!(found.is_expired(), "만료된 키는 is_expired()가 true여야 한다");
        assert_eq!(found.key_id, key.key_id);
    }

    #[tokio::test]
    async fn test_manager_workspace_access_check() {
        let (_tmp, manager) = setup_manager().await;

        let (_, raw) = manager.create_key(
            "WS Check".to_string(),
            vec!["allowed-ws".to_string()],
            Permission::ReadWrite,
            None,
        ).await.unwrap();

        let hashed = hash_key(&raw);
        let key = manager.find_by_hash(&hashed).await.unwrap();

        assert!(key.has_workspace_access("allowed-ws"));
        assert!(!key.has_workspace_access("denied-ws"));
    }

    #[tokio::test]
    async fn test_manager_serialization_roundtrip() {
        let (tmp, manager) = setup_manager().await;

        let expires = Utc::now() + Duration::days(30);
        manager.create_key(
            "Roundtrip".to_string(),
            vec!["ws-a".to_string(), "ws-b".to_string()],
            Permission::ReadWrite,
            Some(expires),
        ).await.unwrap();

        // 디스크에서 재로드
        let manager2 = ApiKeyManager::new(tmp.path().to_str().unwrap()).await.unwrap();
        let keys = manager2.list_keys().await;
        assert_eq!(keys.len(), 1);

        let key = &keys[0];
        assert_eq!(key.label, "Roundtrip");
        assert_eq!(key.workspaces, vec!["ws-a", "ws-b"]);
        assert_eq!(key.permissions, Permission::ReadWrite);
        assert!(key.expires_at.is_some());
    }
}
