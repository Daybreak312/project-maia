use anyhow::{anyhow, Context, Result};
use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::OnceLock;
use tokio::fs;
use tokio::sync::RwLock;
use uuid::Uuid;

// ──────────────────────────────────────────────────────────────
// User
// ──────────────────────────────────────────────────────────────

/// 저장되는 계정 구조체.
/// 비밀번호 평문은 저장하지 않고, argon2id PHC 문자열만 보관한다.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    /// 계정 식별자 (prefix `user_`)
    pub user_id: String,
    /// 로그인 이름 (unique, 소문자 정규화 저장)
    pub username: String,
    /// argon2id PHC 문자열 (`$argon2id$...`)
    pub password_hash: String,
    /// 표시 이름
    pub display_name: String,
    /// 글로벌 관리자 여부 (시스템 전체 admin — 마스터키와 동급 인가)
    pub is_admin: bool,
    pub created_at: DateTime<Utc>,
}

// ──────────────────────────────────────────────────────────────
// Username / Password helpers
// ──────────────────────────────────────────────────────────────

/// username 최대 길이. 개인 워크스페이스 id가 `u-{username}`이므로
/// 워크스페이스 id 상한(64)보다 충분히 짧게 잡는다.
const MAX_USERNAME_LEN: usize = 32;
/// 비밀번호 최소 길이 (기본 위생 — 정책 강화는 Phase 2)
const MIN_PASSWORD_LEN: usize = 8;
/// 비밀번호 최대 길이 (해싱 비용 폭주 방지)
const MAX_PASSWORD_LEN: usize = 128;

/// username을 정규화한다 (trim + 소문자).
/// 저장·조회 모두 이 정규화를 거치므로 대소문자만 다른 중복 계정은 존재할 수 없다.
pub fn normalize_username(raw: &str) -> String {
    raw.trim().to_lowercase()
}

/// username 유효성 검사 (정규화 이후 값 기준).
///
/// 개인 워크스페이스 id `u-{username}`가 워크스페이스 id 규칙
/// (영숫자/하이픈/언더스코어)을 반드시 통과해야 하므로, 여기서 같은 문자 집합으로
/// 제한한다. 첫 글자는 영숫자만 허용해 `-x` 같은 혼동스러운 이름을 막는다.
pub fn validate_username(username: &str) -> Result<(), String> {
    if username.is_empty() {
        return Err("Username cannot be empty".to_string());
    }
    if username.len() > MAX_USERNAME_LEN {
        return Err(format!("Username too long (max {} chars)", MAX_USERNAME_LEN));
    }
    if !username
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(
            "Username can only contain lowercase alphanumeric characters, hyphens, and underscores"
                .to_string(),
        );
    }
    if !username.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()) {
        return Err("Username must start with an alphanumeric character".to_string());
    }
    Ok(())
}

/// 비밀번호 유효성 검사 (길이만 — 복잡도 정책은 도입하지 않는다)
pub fn validate_password(password: &str) -> Result<(), String> {
    if password.len() < MIN_PASSWORD_LEN {
        return Err(format!("Password too short (min {} chars)", MIN_PASSWORD_LEN));
    }
    if password.len() > MAX_PASSWORD_LEN {
        return Err(format!("Password too long (max {} chars)", MAX_PASSWORD_LEN));
    }
    Ok(())
}

/// 비밀번호를 argon2id로 해싱한다 (PHC 문자열 반환).
///
/// API 키와 달리 비밀번호는 저엔트로피 입력이라 SHA-256이 부적합하다 —
/// argon2id(메모리 하드)로 오프라인 무차별 대입 비용을 강제한다.
/// 파라미터는 argon2 crate 기본값(Argon2id v19, m=19456KiB, t=2, p=1 —
/// OWASP 권장 수준)을 사용한다.
pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow!("Failed to hash password: {}", e))?;
    Ok(hash.to_string())
}

/// 비밀번호를 PHC 해시 문자열과 대조한다.
pub fn verify_password_hash(password: &str, phc_hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(phc_hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// 존재하지 않는 username 로그인 시에도 argon2 검증 1회를 수행해
/// 응답 시간으로 계정 존재 여부가 드러나는 것을 완화하기 위한 더미 해시.
fn dummy_password_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| {
        hash_password("maia-dummy-password-for-timing")
            .expect("dummy password hashing must not fail")
    })
}

/// 고유 user_id 생성 (`user_` + 16자 hex)
pub fn generate_user_id() -> String {
    let uuid = Uuid::new_v4().to_string().replace('-', "");
    format!("user_{}", &uuid[..16])
}

// ──────────────────────────────────────────────────────────────
// UserManager — 계정 CRUD + 파일 시스템 영속화
// ──────────────────────────────────────────────────────────────

/// 계정 관리자. `data/users.json`에 계정을 영속화하고 메모리 캐시를 유지한다.
pub struct UserManager {
    users_path: PathBuf,
    users: RwLock<Vec<User>>,
    /// 저장 직렬화 락. 동시 `save()`가 temp 파일/쓰기 순서를 침범하지 않도록 하고,
    /// 락 안에서 최신 인메모리 상태를 다시 읽어 삭제 직후 잔존 스냅샷 부활을 막는다.
    save_lock: tokio::sync::Mutex<()>,
}

impl UserManager {
    /// `data_dir/users.json`에서 계정 목록을 로드한다.
    /// 파일이 없으면 빈 목록으로 시작한다.
    pub async fn new(data_dir: &str) -> Result<Self> {
        let users_path = PathBuf::from(data_dir).join("users.json");

        let users = if users_path.exists() {
            let content = fs::read_to_string(&users_path)
                .await
                .context("Failed to read users.json")?;
            match serde_json::from_str::<Vec<User>>(&content) {
                Ok(users) => users,
                Err(e) => {
                    // 손상된 계정 파일로 부팅을 막지 않는다: 손상본을 백업하고 빈 목록으로
                    // degrade한다(torn write/재배포 크래시 대비). 침묵 금지 — error로
                    // 명시하고, 마스터키(MAIA_API_KEY)로 복구할 수 있게 남긴다.
                    let file_name = users_path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("users.json");
                    let backup = users_path.with_file_name(format!("{}.corrupt", file_name));
                    if let Err(re) = fs::rename(&users_path, &backup).await {
                        tracing::error!("손상된 users.json 백업 실패: {}", re);
                    }
                    tracing::error!(
                        "users.json 파싱 실패({}). 손상 파일을 {:?}로 백업하고 빈 계정 \
                         목록으로 시작합니다. 마스터키(MAIA_API_KEY)로 접속해 계정을 \
                         재생성하세요.",
                        e,
                        backup
                    );
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        Ok(Self {
            users_path,
            users: RwLock::new(users),
            save_lock: tokio::sync::Mutex::new(()),
        })
    }

    /// 새 계정을 생성한다. username은 소문자로 정규화되어 저장된다.
    ///
    /// argon2id 해싱은 CPU 집약적이라 `spawn_blocking`으로 수행해
    /// 런타임 워커를 블로킹하지 않는다.
    pub async fn create_user(
        &self,
        username: &str,
        password: &str,
        display_name: String,
        is_admin: bool,
    ) -> Result<User> {
        let username = normalize_username(username);
        validate_username(&username).map_err(|e| anyhow!(e))?;
        validate_password(password).map_err(|e| anyhow!(e))?;

        // 중복 검사 (정규화 저장이므로 case-insensitive 중복도 걸러진다)
        {
            let users = self.users.read().await;
            if users.iter().any(|u| u.username == username) {
                return Err(anyhow!("Username '{}' already exists", username));
            }
        }

        let password = password.to_string();
        let password_hash = tokio::task::spawn_blocking(move || hash_password(&password))
            .await
            .context("Password hashing task panicked")??;

        let user = User {
            user_id: generate_user_id(),
            username: username.clone(),
            password_hash,
            display_name,
            is_admin,
            created_at: Utc::now(),
        };

        {
            let mut users = self.users.write().await;
            // 해싱하는 동안 동일 username이 먼저 들어왔을 수 있다 — 쓰기 락 안에서 재확인.
            if users.iter().any(|u| u.username == username) {
                return Err(anyhow!("Username '{}' already exists", username));
            }
            users.push(user.clone());
        }

        self.save().await?;

        tracing::info!("Created user: {} ({})", user.username, user.user_id);
        Ok(user)
    }

    /// username/password로 계정을 인증한다. 실패 시 `None` (사유 비구분 — 열거 방지).
    ///
    /// 존재하지 않는 username이어도 더미 argon2 검증을 수행해 타이밍으로
    /// 계정 존재 여부가 드러나는 것을 완화한다.
    pub async fn verify_password(&self, username: &str, password: &str) -> Option<User> {
        let username = normalize_username(username);
        let user = {
            let users = self.users.read().await;
            users.iter().find(|u| u.username == username).cloned()
        };

        let password = password.to_string();
        match user {
            Some(user) => {
                let hash = user.password_hash.clone();
                let ok = tokio::task::spawn_blocking(move || verify_password_hash(&password, &hash))
                    .await
                    .unwrap_or(false);
                if ok {
                    Some(user)
                } else {
                    None
                }
            }
            None => {
                // 타이밍 균등화용 더미 검증 (결과는 항상 실패)
                let _ = tokio::task::spawn_blocking(move || {
                    verify_password_hash(&password, dummy_password_hash())
                })
                .await;
                None
            }
        }
    }

    /// user_id로 계정을 조회한다.
    pub async fn get(&self, user_id: &str) -> Option<User> {
        let users = self.users.read().await;
        users.iter().find(|u| u.user_id == user_id).cloned()
    }

    /// username(정규화 후)으로 계정을 조회한다.
    pub async fn get_by_username(&self, username: &str) -> Option<User> {
        let username = normalize_username(username);
        let users = self.users.read().await;
        users.iter().find(|u| u.username == username).cloned()
    }

    /// 저장된 모든 계정 목록을 반환한다.
    pub async fn list_users(&self) -> Vec<User> {
        self.users.read().await.clone()
    }

    /// 계정을 삭제하고, 삭제된 계정을 반환한다 (연쇄 정리를 호출측이 수행할 수 있게).
    pub async fn delete_user(&self, user_id: &str) -> Result<User> {
        let removed = {
            let mut users = self.users.write().await;
            let idx = users
                .iter()
                .position(|u| u.user_id == user_id)
                .ok_or_else(|| anyhow!("User not found: {}", user_id))?;
            users.remove(idx)
        };

        self.save().await?;

        tracing::info!("Deleted user: {} ({})", removed.username, removed.user_id);
        Ok(removed)
    }

    /// 비밀번호를 변경한다. 세션 폐기 등 연쇄 조치는 호출측 책임.
    pub async fn set_password(&self, user_id: &str, new_password: &str) -> Result<()> {
        validate_password(new_password).map_err(|e| anyhow!(e))?;

        let new_password = new_password.to_string();
        let password_hash = tokio::task::spawn_blocking(move || hash_password(&new_password))
            .await
            .context("Password hashing task panicked")??;

        {
            let mut users = self.users.write().await;
            let user = users
                .iter_mut()
                .find(|u| u.user_id == user_id)
                .ok_or_else(|| anyhow!("User not found: {}", user_id))?;
            user.password_hash = password_hash;
        }

        self.save().await?;

        tracing::info!("Password changed for user: {}", user_id);
        Ok(())
    }

    /// 등록된 계정이 있는지 확인한다 (부트스트랩 잠금 안내 판단용).
    pub async fn has_users(&self) -> bool {
        !self.users.read().await.is_empty()
    }

    /// 현재 계정 목록을 디스크에 원자적으로 영속화한다.
    ///
    /// temp 파일에 쓴 뒤 `rename`으로 교체해 torn write(쓰기 도중 크래시로 인한
    /// 잘린 파일 → 부팅 브릭)를 방지한다. `save_lock`으로 동시 저장을 직렬화하고,
    /// 락 안에서 최신 인메모리 상태를 다시 직렬화하므로 삭제 직후 잔존 스냅샷이
    /// 되살아나는 경합도 막는다.
    async fn save(&self) -> Result<()> {
        let _guard = self.save_lock.lock().await;

        let content = {
            let users = self.users.read().await;
            serde_json::to_string_pretty(&*users)?
        };

        if let Some(parent) = self.users_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        let file_name = self
            .users_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("users.json");
        let tmp_path = self.users_path.with_file_name(format!("{}.tmp", file_name));

        fs::write(&tmp_path, content)
            .await
            .context("Failed to write users.json.tmp")?;
        fs::rename(&tmp_path, &self.users_path)
            .await
            .context("Failed to persist users.json")?;

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

    // ──── normalize / validate ────

    #[test]
    fn test_normalize_username_lowercases_and_trims() {
        assert_eq!(normalize_username("  Alice "), "alice");
        assert_eq!(normalize_username("BOB"), "bob");
        assert_eq!(normalize_username("charlie"), "charlie");
    }

    #[test]
    fn test_validate_username_accepts_valid() {
        assert!(validate_username("alice").is_ok());
        assert!(validate_username("bob-123").is_ok());
        assert!(validate_username("under_score").is_ok());
        assert!(validate_username("a").is_ok());
        assert!(validate_username("0numeric").is_ok());
    }

    #[test]
    fn test_validate_username_rejects_invalid() {
        assert!(validate_username("").is_err());
        assert!(validate_username("has space").is_err());
        assert!(validate_username("UPPER").is_err()); // 정규화 이후 값 기준이므로 대문자는 거부
        assert!(validate_username("-leading").is_err());
        assert!(validate_username("_leading").is_err());
        assert!(validate_username("한글이름").is_err());
        assert!(validate_username(&"x".repeat(33)).is_err());
    }

    #[test]
    fn test_validate_username_fits_workspace_id() {
        // 개인 워크스페이스 `u-{username}`가 워크스페이스 id 규칙을 통과해야 한다.
        use crate::workspace::validate_workspace_id;
        for name in ["alice", "bob-123", "under_score", &"x".repeat(32)] {
            validate_username(name).unwrap();
            validate_workspace_id(&format!("u-{}", name)).unwrap();
        }
    }

    #[test]
    fn test_validate_password_length_bounds() {
        assert!(validate_password("short").is_err());
        assert!(validate_password("12345678").is_ok());
        assert!(validate_password(&"x".repeat(128)).is_ok());
        assert!(validate_password(&"x".repeat(129)).is_err());
    }

    // ──── hash_password / verify_password_hash ────

    #[test]
    fn test_hash_password_is_argon2id_phc() {
        let hash = hash_password("my-password-123").unwrap();
        assert!(
            hash.starts_with("$argon2id$"),
            "비밀번호 해시는 argon2id PHC 문자열이어야 한다: {}",
            hash
        );
    }

    #[test]
    fn test_hash_password_salted_differs() {
        // 같은 비밀번호라도 salt가 달라 해시가 달라야 한다 (rainbow table 방지)
        let h1 = hash_password("same-password").unwrap();
        let h2 = hash_password("same-password").unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_verify_password_hash_roundtrip() {
        let hash = hash_password("correct horse battery staple").unwrap();
        assert!(verify_password_hash("correct horse battery staple", &hash));
        assert!(!verify_password_hash("wrong password", &hash));
    }

    #[test]
    fn test_verify_password_hash_rejects_malformed_hash() {
        assert!(!verify_password_hash("any", "not-a-phc-string"));
        assert!(!verify_password_hash("any", ""));
        // SHA-256 해시(API 키용 포맷)는 비밀번호 해시로 인정되지 않는다
        assert!(!verify_password_hash("any", "sha256:abcdef"));
    }

    // ──── generate_user_id ────

    #[test]
    fn test_generate_user_id_prefix_and_length() {
        let id = generate_user_id();
        assert!(id.starts_with("user_"));
        // "user_" (5) + 16 hex = 21
        assert_eq!(id.len(), 21);
    }

    #[test]
    fn test_generate_user_id_unique() {
        assert_ne!(generate_user_id(), generate_user_id());
    }

    // ──── UserManager ────

    async fn setup_manager() -> (TempDir, UserManager) {
        let tmp = TempDir::new().unwrap();
        let manager = UserManager::new(tmp.path().to_str().unwrap()).await.unwrap();
        (tmp, manager)
    }

    #[tokio::test]
    async fn test_manager_create_user() {
        let (_tmp, manager) = setup_manager().await;

        let user = manager
            .create_user("Alice", "password123", "Alice Kim".to_string(), false)
            .await
            .unwrap();

        assert!(user.user_id.starts_with("user_"));
        assert_eq!(user.username, "alice", "username은 소문자로 정규화 저장");
        assert_eq!(user.display_name, "Alice Kim");
        assert!(!user.is_admin);
        assert!(user.password_hash.starts_with("$argon2id$"));
    }

    #[tokio::test]
    async fn test_manager_create_duplicate_username_fails() {
        let (_tmp, manager) = setup_manager().await;

        manager
            .create_user("alice", "password123", "A".to_string(), false)
            .await
            .unwrap();
        // 대소문자만 다른 중복도 거부되어야 한다 (정규화 저장)
        let err = manager
            .create_user("ALICE", "password456", "B".to_string(), false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[tokio::test]
    async fn test_manager_create_invalid_username_fails() {
        let (_tmp, manager) = setup_manager().await;
        assert!(manager
            .create_user("bad name", "password123", "X".to_string(), false)
            .await
            .is_err());
        assert!(manager
            .create_user("", "password123", "X".to_string(), false)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn test_manager_create_short_password_fails() {
        let (_tmp, manager) = setup_manager().await;
        let err = manager
            .create_user("alice", "short", "X".to_string(), false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("too short"));
    }

    #[tokio::test]
    async fn test_manager_verify_password() {
        let (_tmp, manager) = setup_manager().await;
        manager
            .create_user("alice", "password123", "A".to_string(), false)
            .await
            .unwrap();

        // 올바른 자격증명 (username 대소문자 무관)
        assert!(manager.verify_password("alice", "password123").await.is_some());
        assert!(manager.verify_password("ALICE", "password123").await.is_some());

        // 잘못된 비밀번호 / 없는 계정 — 둘 다 동일하게 None (사유 비구분)
        assert!(manager.verify_password("alice", "wrong-password").await.is_none());
        assert!(manager.verify_password("nobody", "password123").await.is_none());
    }

    #[tokio::test]
    async fn test_manager_get_and_get_by_username() {
        let (_tmp, manager) = setup_manager().await;
        let created = manager
            .create_user("alice", "password123", "A".to_string(), true)
            .await
            .unwrap();

        let by_id = manager.get(&created.user_id).await.unwrap();
        assert_eq!(by_id.username, "alice");
        assert!(by_id.is_admin);

        let by_name = manager.get_by_username("Alice").await.unwrap();
        assert_eq!(by_name.user_id, created.user_id);

        assert!(manager.get("user_nonexistent00").await.is_none());
        assert!(manager.get_by_username("nobody").await.is_none());
    }

    #[tokio::test]
    async fn test_manager_delete_user() {
        let (_tmp, manager) = setup_manager().await;
        let user = manager
            .create_user("alice", "password123", "A".to_string(), false)
            .await
            .unwrap();

        let removed = manager.delete_user(&user.user_id).await.unwrap();
        assert_eq!(removed.user_id, user.user_id);
        assert!(manager.get(&user.user_id).await.is_none());
        assert!(!manager.has_users().await);

        // 삭제된 계정으로는 더 이상 로그인할 수 없다
        assert!(manager.verify_password("alice", "password123").await.is_none());
    }

    #[tokio::test]
    async fn test_manager_delete_nonexistent_fails() {
        let (_tmp, manager) = setup_manager().await;
        let err = manager.delete_user("user_ghost").await.unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn test_manager_set_password() {
        let (_tmp, manager) = setup_manager().await;
        let user = manager
            .create_user("alice", "old-password", "A".to_string(), false)
            .await
            .unwrap();

        manager.set_password(&user.user_id, "new-password-456").await.unwrap();

        assert!(manager.verify_password("alice", "old-password").await.is_none());
        assert!(manager.verify_password("alice", "new-password-456").await.is_some());
    }

    #[tokio::test]
    async fn test_manager_set_password_validates_length() {
        let (_tmp, manager) = setup_manager().await;
        let user = manager
            .create_user("alice", "password123", "A".to_string(), false)
            .await
            .unwrap();
        assert!(manager.set_password(&user.user_id, "short").await.is_err());
    }

    #[tokio::test]
    async fn test_manager_has_users() {
        let (_tmp, manager) = setup_manager().await;
        assert!(!manager.has_users().await);
        manager
            .create_user("alice", "password123", "A".to_string(), false)
            .await
            .unwrap();
        assert!(manager.has_users().await);
    }

    #[tokio::test]
    async fn test_manager_persists_to_disk() {
        let (tmp, manager) = setup_manager().await;
        manager
            .create_user("alice", "password123", "Alice".to_string(), true)
            .await
            .unwrap();

        // 새 매니저로 디스크에서 재로드
        let manager2 = UserManager::new(tmp.path().to_str().unwrap()).await.unwrap();
        let users = manager2.list_users().await;
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].username, "alice");
        assert!(users[0].is_admin);

        // 재로드 후에도 비밀번호 검증이 동작해야 한다 (해시 영속화 확인)
        assert!(manager2.verify_password("alice", "password123").await.is_some());
    }

    #[tokio::test]
    async fn test_manager_plaintext_never_persisted() {
        let (tmp, manager) = setup_manager().await;
        manager
            .create_user("alice", "super-secret-password", "A".to_string(), false)
            .await
            .unwrap();

        let content = fs::read_to_string(tmp.path().join("users.json")).await.unwrap();
        assert!(
            !content.contains("super-secret-password"),
            "비밀번호 평문이 디스크에 남으면 안 된다"
        );
        assert!(content.contains("$argon2id$"));
    }

    #[tokio::test]
    async fn test_manager_corrupt_file_degrades_gracefully() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("users.json");
        // 손상된(파싱 불가) JSON 기록 — torn write 시뮬레이션
        fs::write(&path, "{ this is not valid json ]").await.unwrap();

        // 부팅이 막히지 않고 빈 목록으로 degrade해야 한다 (하드 실패 아님)
        let manager = UserManager::new(tmp.path().to_str().unwrap()).await.unwrap();
        assert!(manager.list_users().await.is_empty());

        // 손상 파일은 .corrupt로 백업되어 보존된다 (복구 가능)
        let backup = tmp.path().join("users.json.corrupt");
        assert!(backup.exists(), "손상 파일은 .corrupt로 백업되어야 한다");
    }

    #[tokio::test]
    async fn test_manager_save_is_atomic_no_temp_left() {
        let (tmp, manager) = setup_manager().await;
        manager
            .create_user("alice", "password123", "A".to_string(), false)
            .await
            .unwrap();

        let tmp_path = tmp.path().join("users.json.tmp");
        assert!(!tmp_path.exists(), "저장 후 .tmp 파일이 남으면 안 된다");
    }
}
