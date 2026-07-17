use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::fs;
use tokio::sync::RwLock;

use super::keys::hash_key;

/// 세션 쿠키 이름. 미들웨어(Cookie 헤더 파싱)와 로그인/로그아웃 핸들러
/// (Set-Cookie 발급)가 공유하는 단일 계약.
pub const SESSION_COOKIE_NAME: &str = "maia_session";

/// 세션 수명(30일). 쿠키 Max-Age(2592000초)와 서버측 만료가 같은 값을 가리킨다.
pub const SESSION_TTL_DAYS: i64 = 30;

/// 세션 토큰 원문 길이(바이트). CSPRNG 32바이트 → hex 64자.
const SESSION_TOKEN_BYTES: usize = 32;

// ──────────────────────────────────────────────────────────────
// Session
// ──────────────────────────────────────────────────────────────

/// 저장되는 세션 구조체.
/// 토큰 원문은 저장하지 않고, SHA-256 해시만 보관한다 —
/// sessions.json이 유출되어도 세션 탈취로 이어지지 않게 한다.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// 토큰 SHA-256 해시 (`sha256:` prefix — API 키와 동일 포맷)
    pub token_hash: String,
    /// 세션 소유 계정 (User.user_id)
    pub user_id: String,
    pub created_at: DateTime<Utc>,
    /// 만료 시각 (created_at + 30일 고정 — 슬라이딩 연장 없음)
    pub expires_at: DateTime<Utc>,
}

impl Session {
    /// 세션이 만료되었는지 확인
    pub fn is_expired(&self) -> bool {
        Utc::now() > self.expires_at
    }
}

/// CSPRNG로 세션 토큰 원문을 생성한다 (32바이트 → 64자 hex).
///
/// uuid(v4)는 122비트 랜덤이지만 "식별자" 용도로 설계된 물건이라
/// 세션 토큰에는 OS 엔트로피를 직접 쓴다 (요구사항: uuid 조합 금지).
pub fn generate_session_token() -> String {
    let mut bytes = [0u8; SESSION_TOKEN_BYTES];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

// ──────────────────────────────────────────────────────────────
// SessionManager — 세션 CRUD + 파일 시스템 영속화
// ──────────────────────────────────────────────────────────────

/// 세션 관리자. `data/sessions.json`에 세션을 영속화해 서버 재시작에도
/// 로그인이 유지되게 한다. 만료 세션은 로드/변경 시점에 청소한다.
pub struct SessionManager {
    sessions_path: PathBuf,
    sessions: RwLock<Vec<Session>>,
    /// 저장 직렬화 락 (ApiKeyManager와 동일한 원자 저장 패턴)
    save_lock: tokio::sync::Mutex<()>,
}

impl SessionManager {
    /// `data_dir/sessions.json`에서 세션 목록을 로드한다.
    /// 파일이 없으면 빈 목록으로 시작하고, 만료분은 로드 시점에 청소한다.
    pub async fn new(data_dir: &str) -> Result<Self> {
        let sessions_path = PathBuf::from(data_dir).join("sessions.json");

        let mut sessions = if sessions_path.exists() {
            let content = fs::read_to_string(&sessions_path)
                .await
                .context("Failed to read sessions.json")?;
            match serde_json::from_str::<Vec<Session>>(&content) {
                Ok(sessions) => sessions,
                Err(e) => {
                    // 손상된 세션 파일로 부팅을 막지 않는다. 세션은 재로그인으로
                    // 복구 가능한 소모품이므로 백업 후 빈 목록 degrade가 안전하다.
                    let file_name = sessions_path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("sessions.json");
                    let backup = sessions_path.with_file_name(format!("{}.corrupt", file_name));
                    if let Err(re) = fs::rename(&sessions_path, &backup).await {
                        tracing::error!("손상된 sessions.json 백업 실패: {}", re);
                    }
                    tracing::error!(
                        "sessions.json 파싱 실패({}). 손상 파일을 {:?}로 백업하고 빈 세션 \
                         목록으로 시작합니다. 사용자들은 재로그인이 필요합니다.",
                        e,
                        backup
                    );
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        // 만료분 청소 (로드 시)
        let before = sessions.len();
        sessions.retain(|s| !s.is_expired());
        let purged = before - sessions.len();

        let manager = Self {
            sessions_path,
            sessions: RwLock::new(sessions),
            save_lock: tokio::sync::Mutex::new(()),
        };

        // 청소가 있었으면 디스크에도 반영 (만료 해시가 파일에 남지 않게)
        if purged > 0 {
            tracing::info!("Purged {} expired session(s) on load", purged);
            manager.save().await?;
        }

        Ok(manager)
    }

    /// 새 세션을 발급한다. (Session, 평문 토큰)을 반환한다.
    /// 평문 토큰은 이 시점에서만 확인 가능하며, 이후에는 해시만 저장된다.
    pub async fn create_session(&self, user_id: &str) -> Result<(Session, String)> {
        let raw_token = generate_session_token();
        let now = Utc::now();

        let session = Session {
            token_hash: hash_key(&raw_token),
            user_id: user_id.to_string(),
            created_at: now,
            expires_at: now + Duration::days(SESSION_TTL_DAYS),
        };

        {
            let mut sessions = self.sessions.write().await;
            // 변경 시점마다 만료분을 함께 청소해 파일이 무한히 자라지 않게 한다
            sessions.retain(|s| !s.is_expired());
            sessions.push(session.clone());
        }

        self.save().await?;

        tracing::info!("Created session for user: {}", user_id);
        Ok((session, raw_token))
    }

    /// 평문 토큰으로 세션을 인증한다. 만료되었거나 없는 토큰은 `None`.
    pub async fn authenticate(&self, raw_token: &str) -> Option<Session> {
        let hashed = hash_key(raw_token);
        let sessions = self.sessions.read().await;
        sessions
            .iter()
            .find(|s| s.token_hash == hashed && !s.is_expired())
            .cloned()
    }

    /// 평문 토큰으로 세션을 폐기한다 (로그아웃).
    /// 존재하지 않아도 성공으로 처리한다 (멱등 — 재로그아웃/만료 후 로그아웃 허용).
    pub async fn revoke(&self, raw_token: &str) -> Result<bool> {
        let hashed = hash_key(raw_token);
        let removed = {
            let mut sessions = self.sessions.write().await;
            let before = sessions.len();
            sessions.retain(|s| s.token_hash != hashed && !s.is_expired());
            before != sessions.len()
        };

        if removed {
            self.save().await?;
        }
        Ok(removed)
    }

    /// 특정 계정의 모든 세션을 폐기한다 (계정 삭제·비밀번호 변경 연쇄 조치).
    pub async fn revoke_all_for_user(&self, user_id: &str) -> Result<usize> {
        let removed = {
            let mut sessions = self.sessions.write().await;
            let before = sessions.len();
            sessions.retain(|s| s.user_id != user_id && !s.is_expired());
            before - sessions.len()
        };

        if removed > 0 {
            self.save().await?;
            tracing::info!("Revoked {} session(s) for user: {}", removed, user_id);
        }
        Ok(removed)
    }

    /// 저장된 세션 수 (테스트·관측용)
    pub async fn count(&self) -> usize {
        self.sessions.read().await.len()
    }

    /// 현재 세션 목록을 디스크에 원자적으로 영속화한다.
    /// (temp+rename, save_lock 직렬화 — ApiKeyManager와 동일 패턴)
    async fn save(&self) -> Result<()> {
        let _guard = self.save_lock.lock().await;

        let content = {
            let sessions = self.sessions.read().await;
            serde_json::to_string_pretty(&*sessions)?
        };

        if let Some(parent) = self.sessions_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        let file_name = self
            .sessions_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("sessions.json");
        let tmp_path = self
            .sessions_path
            .with_file_name(format!("{}.tmp", file_name));

        fs::write(&tmp_path, content)
            .await
            .context("Failed to write sessions.json.tmp")?;
        fs::rename(&tmp_path, &self.sessions_path)
            .await
            .context("Failed to persist sessions.json")?;

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

    async fn setup_manager() -> (TempDir, SessionManager) {
        let tmp = TempDir::new().unwrap();
        let manager = SessionManager::new(tmp.path().to_str().unwrap()).await.unwrap();
        (tmp, manager)
    }

    // ──── generate_session_token ────

    #[test]
    fn test_generate_session_token_length() {
        let token = generate_session_token();
        // 32바이트 → 64자 hex
        assert_eq!(token.len(), 64);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_generate_session_token_unique() {
        assert_ne!(generate_session_token(), generate_session_token());
    }

    // ──── create / authenticate ────

    #[tokio::test]
    async fn test_create_and_authenticate() {
        let (_tmp, manager) = setup_manager().await;

        let (session, raw) = manager.create_session("user_abc").await.unwrap();
        assert_eq!(session.user_id, "user_abc");
        assert_eq!(session.token_hash, hash_key(&raw));

        let authed = manager.authenticate(&raw).await.unwrap();
        assert_eq!(authed.user_id, "user_abc");
    }

    #[tokio::test]
    async fn test_authenticate_wrong_token() {
        let (_tmp, manager) = setup_manager().await;
        manager.create_session("user_abc").await.unwrap();

        assert!(manager.authenticate("wrong-token").await.is_none());
        assert!(manager.authenticate("").await.is_none());
    }

    #[tokio::test]
    async fn test_session_ttl_is_30_days() {
        let (_tmp, manager) = setup_manager().await;
        let (session, _) = manager.create_session("user_abc").await.unwrap();

        let ttl = session.expires_at - session.created_at;
        assert_eq!(ttl.num_days(), SESSION_TTL_DAYS);
    }

    #[tokio::test]
    async fn test_expired_session_rejected() {
        let (_tmp, manager) = setup_manager().await;
        let (_, raw) = manager.create_session("user_abc").await.unwrap();

        // 만료 시각을 과거로 조작
        {
            let mut sessions = manager.sessions.write().await;
            sessions[0].expires_at = Utc::now() - Duration::seconds(1);
        }

        assert!(
            manager.authenticate(&raw).await.is_none(),
            "만료된 세션은 인증되면 안 된다"
        );
    }

    // ──── revoke ────

    #[tokio::test]
    async fn test_revoke_session() {
        let (_tmp, manager) = setup_manager().await;
        let (_, raw) = manager.create_session("user_abc").await.unwrap();

        assert!(manager.revoke(&raw).await.unwrap());
        assert!(manager.authenticate(&raw).await.is_none(), "폐기된 세션은 인증되면 안 된다");

        // 재폐기는 멱등 (false 반환, 에러 아님)
        assert!(!manager.revoke(&raw).await.unwrap());
    }

    #[tokio::test]
    async fn test_revoke_all_for_user() {
        let (_tmp, manager) = setup_manager().await;
        let (_, raw1) = manager.create_session("user_a").await.unwrap();
        let (_, raw2) = manager.create_session("user_a").await.unwrap();
        let (_, raw_b) = manager.create_session("user_b").await.unwrap();

        let removed = manager.revoke_all_for_user("user_a").await.unwrap();
        assert_eq!(removed, 2);

        assert!(manager.authenticate(&raw1).await.is_none());
        assert!(manager.authenticate(&raw2).await.is_none());
        // 다른 계정 세션은 영향 없음
        assert!(manager.authenticate(&raw_b).await.is_some());
    }

    // ──── 영속화 ────

    #[tokio::test]
    async fn test_persists_across_restart() {
        let (tmp, manager) = setup_manager().await;
        let (_, raw) = manager.create_session("user_abc").await.unwrap();

        // 재시작 시뮬레이션: 새 매니저로 디스크에서 재로드
        let manager2 = SessionManager::new(tmp.path().to_str().unwrap()).await.unwrap();
        let authed = manager2.authenticate(&raw).await;
        assert!(authed.is_some(), "재시작 후에도 세션이 유지되어야 한다 (로그인 유지)");
        assert_eq!(authed.unwrap().user_id, "user_abc");
    }

    #[tokio::test]
    async fn test_raw_token_never_persisted() {
        let (tmp, manager) = setup_manager().await;
        let (_, raw) = manager.create_session("user_abc").await.unwrap();

        let content = fs::read_to_string(tmp.path().join("sessions.json")).await.unwrap();
        assert!(
            !content.contains(&raw),
            "토큰 원문이 디스크에 남으면 안 된다 (해시만 저장)"
        );
        assert!(content.contains("sha256:"));
    }

    #[tokio::test]
    async fn test_expired_sessions_purged_on_load() {
        let (tmp, manager) = setup_manager().await;
        let (_, raw_live) = manager.create_session("user_live").await.unwrap();
        manager.create_session("user_dead").await.unwrap();

        // 두 번째 세션을 만료시켜 디스크에 기록
        {
            let mut sessions = manager.sessions.write().await;
            sessions[1].expires_at = Utc::now() - Duration::seconds(1);
        }
        manager.save().await.unwrap();

        // 재로드 시 만료분이 청소되어야 한다 (메모리 + 디스크 모두)
        let manager2 = SessionManager::new(tmp.path().to_str().unwrap()).await.unwrap();
        assert_eq!(manager2.count().await, 1);
        assert!(manager2.authenticate(&raw_live).await.is_some());

        let content = fs::read_to_string(tmp.path().join("sessions.json")).await.unwrap();
        let on_disk: Vec<Session> = serde_json::from_str(&content).unwrap();
        assert_eq!(on_disk.len(), 1, "만료 세션은 디스크에서도 청소되어야 한다");
    }

    #[tokio::test]
    async fn test_create_purges_expired() {
        let (_tmp, manager) = setup_manager().await;
        manager.create_session("user_old").await.unwrap();
        {
            let mut sessions = manager.sessions.write().await;
            sessions[0].expires_at = Utc::now() - Duration::seconds(1);
        }

        // 새 세션 생성 시 만료분이 함께 청소된다
        manager.create_session("user_new").await.unwrap();
        assert_eq!(manager.count().await, 1);
    }

    #[tokio::test]
    async fn test_corrupt_file_degrades_gracefully() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("sessions.json");
        fs::write(&path, "{ broken json !!").await.unwrap();

        let manager = SessionManager::new(tmp.path().to_str().unwrap()).await.unwrap();
        assert_eq!(manager.count().await, 0);

        let backup = tmp.path().join("sessions.json.corrupt");
        assert!(backup.exists(), "손상 파일은 .corrupt로 백업되어야 한다");
    }

    #[tokio::test]
    async fn test_save_is_atomic_no_temp_left() {
        let (tmp, manager) = setup_manager().await;
        manager.create_session("user_abc").await.unwrap();

        let tmp_path = tmp.path().join("sessions.json.tmp");
        assert!(!tmp_path.exists(), "저장 후 .tmp 파일이 남으면 안 된다");
    }
}
