use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
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

    /// 권한 서열 (ReadOnly < ReadWrite < Admin).
    /// derive(Ord)의 선언 순서 의존 대신 명시적 서열로 고정한다.
    fn rank(&self) -> u8 {
        match self {
            Permission::ReadOnly => 0,
            Permission::ReadWrite => 1,
            Permission::Admin => 2,
        }
    }

    /// 두 권한의 교집합 — 더 낮은 쪽을 반환한다.
    /// 소유 계정이 있는 API 키의 유효 권한(키 스코프 ∩ 계정 접근권) 산출에 쓴다.
    pub fn intersect(&self, other: &Permission) -> Permission {
        if self.rank() <= other.rank() {
            self.clone()
        } else {
            other.clone()
        }
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
    /// 소유 계정(User.user_id). None이면 시스템 키 — 기존 의미 그대로
    /// 키 자체 스코프로 동작한다 (`#[serde(default)]`로 기존 키 파일 하위호환).
    /// Some이면 유효 접근권 = 키 스코프 ∩ 소유 계정의 현재 접근권(미들웨어에서
    /// 매 요청 산출 — 계정이 워크스페이스에서 빠지면 키도 그 즉시 접근 불가).
    #[serde(default)]
    pub owner: Option<String>,
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

/// 인증 자격증명의 출처. 데이터 엔드포인트는 이 값을 몰라야 하며(소스 무관 인가),
/// 자격증명 관리 엔드포인트(`/api/me/keys` 세션 전용 게이트, `/api/auth/me` 표시)와
/// 미들웨어(last_used 갱신)만 참조한다.
#[derive(Debug, Clone, PartialEq)]
pub enum AuthSource {
    /// MAIA_API_KEY 마스터키
    Master,
    /// MAIA_DEV_NO_AUTH=1 명시적 개발 모드
    DevMode,
    /// 등록된 API 키 (Bearer)
    ApiKey,
    /// 로그인 세션 쿠키
    Session,
}

/// 인증된 계정의 최소 신원. 세션 인증 또는 소유 계정 있는 API 키 인증 시 채워진다.
#[derive(Debug, Clone)]
pub struct UserIdentity {
    pub user_id: String,
    pub username: String,
}

/// 워크스페이스 접근 스코프 — 인증 주체 유형별 판정 규칙.
///
/// 기존 "단일 권한 + 워크스페이스 목록" 모델은 유저 세션의 "워크스페이스마다
/// 다른 role"을 표현할 수 없어, 판정 규칙 자체를 스코프 타입으로 분리했다.
#[derive(Debug, Clone)]
pub enum WorkspaceScope {
    /// 전 워크스페이스 Admin (마스터키/개발모드/글로벌 admin 계정)
    All,
    /// 고정 목록 + 컨텍스트의 단일 권한 (시스템 API 키 — 기존 의미 그대로.
    /// 빈 목록 = 접근 없음, fail-closed)
    Fixed(Vec<String>),
    /// 워크스페이스별 유효 권한 맵 (유저 세션: 멤버십 ∪ public /
    /// 소유 계정 있는 키: 키 스코프 ∩ 계정 접근권).
    /// BTreeMap이라 순회가 결정적이다 (default_workspace 판정의 예측 가능성).
    PerWorkspace(BTreeMap<String, Permission>),
}

/// 인증된 요청의 컨텍스트. 미들웨어가 검증 후 Request Extensions에 삽입한다.
/// 어떤 인증 경로든 최종적으로 이 타입 하나로 수렴하므로, 하위 핸들러는
/// 인증 소스와 무관하게 동일한 인가 판정 API를 사용한다.
#[derive(Debug, Clone)]
pub struct AuthContext {
    /// 인증 주체 식별자 ("master" | "dev" | 키 key_id | 세션의 user_id)
    pub key_id: String,
    /// 전역(시스템) 권한 — 워크스페이스와 무관한 시스템 작업(require_admin:
    /// 워크스페이스 CRUD, 설정, 키 관리 등) 판정에 쓴다.
    /// 워크스페이스별 유효 권한은 `permission_for()`가 판정한다.
    pub permissions: Permission,
    /// 워크스페이스 접근 스코프
    pub scope: WorkspaceScope,
    /// 마스터키 또는 개발모드 여부
    pub is_master: bool,
    /// 인증 자격증명 출처
    pub source: AuthSource,
    /// 인증된 계정 신원 (세션/소유 키 인증 시)
    pub user: Option<UserIdentity>,
}

impl AuthContext {
    /// 마스터키(MAIA_API_KEY) 인증 시 생성
    pub fn master() -> Self {
        Self {
            key_id: "master".to_string(),
            permissions: Permission::Admin,
            scope: WorkspaceScope::All,
            is_master: true,
            source: AuthSource::Master,
            user: None,
        }
    }

    /// 개발 모드 (MAIA_DEV_NO_AUTH=1 명시적 옵트인) 시 생성
    pub fn dev_mode() -> Self {
        Self {
            key_id: "dev".to_string(),
            permissions: Permission::Admin,
            scope: WorkspaceScope::All,
            is_master: true,
            source: AuthSource::DevMode,
            user: None,
        }
    }

    /// 시스템 API Key(소유 계정 없음)로부터 AuthContext 생성 — 기존 의미 그대로
    /// 키의 워크스페이스 목록 + 단일 권한으로 동작한다.
    pub fn from_api_key(key: &ApiKey) -> Self {
        Self {
            key_id: key.key_id.clone(),
            permissions: key.permissions.clone(),
            scope: WorkspaceScope::Fixed(key.workspaces.clone()),
            is_master: false,
            source: AuthSource::ApiKey,
            user: None,
        }
    }

    /// 소유 계정이 있는 API Key로부터 AuthContext 생성.
    ///
    /// 유효 접근권 = 키 스코프 ∩ 소유 계정의 현재 접근권(`user_access`):
    /// - 워크스페이스 집합: 키 목록 ∩ 계정 접근 가능 목록
    /// - 워크스페이스별 권한: min(키 권한, 계정의 해당 ws 권한)
    /// - 전역 권한: min(키 권한, 계정 전역 권한 — admin 계정만 Admin, 그 외 ReadOnly)
    ///
    /// 계정이 워크스페이스에서 빠지면 이 교집합에서 즉시 사라지므로,
    /// 키가 계정보다 넓은 접근을 가질 수 없다.
    pub fn from_owned_api_key(
        key: &ApiKey,
        user: &crate::auth::User,
        user_access: &BTreeMap<String, Permission>,
    ) -> Self {
        let mut effective = BTreeMap::new();
        for ws in &key.workspaces {
            if let Some(user_perm) = user_access.get(ws) {
                effective.insert(ws.clone(), key.permissions.intersect(user_perm));
            }
        }

        let user_global = if user.is_admin {
            Permission::Admin
        } else {
            Permission::ReadOnly
        };

        Self {
            key_id: key.key_id.clone(),
            permissions: key.permissions.intersect(&user_global),
            scope: WorkspaceScope::PerWorkspace(effective),
            is_master: false,
            source: AuthSource::ApiKey,
            user: Some(UserIdentity {
                user_id: user.user_id.clone(),
                username: user.username.clone(),
            }),
        }
    }

    /// 로그인 세션의 계정으로부터 AuthContext 생성.
    ///
    /// - 글로벌 admin 계정: 전 워크스페이스 Admin (마스터키와 동급 인가 — 단
    ///   `is_master`는 아니므로 "마스터키" 표시/식별과는 구분된다)
    /// - 일반 계정: 접근 가능 ws = 멤버인 ws ∪ public ws (`access` 맵),
    ///   전역 권한은 ReadOnly 바닥 — 시스템 작업(require_admin)은 불가
    pub fn from_user(user: &crate::auth::User, access: BTreeMap<String, Permission>) -> Self {
        let (permissions, scope) = if user.is_admin {
            (Permission::Admin, WorkspaceScope::All)
        } else {
            (Permission::ReadOnly, WorkspaceScope::PerWorkspace(access))
        };

        Self {
            key_id: user.user_id.clone(),
            permissions,
            scope,
            is_master: false,
            source: AuthSource::Session,
            user: Some(UserIdentity {
                user_id: user.user_id.clone(),
                username: user.username.clone(),
            }),
        }
    }

    /// 특정 워크스페이스에 대한 유효 권한을 판정한다.
    ///
    /// - `All`: 항상 Admin
    /// - `Fixed`: 목록에 있으면 컨텍스트의 단일 권한, 없으면 None
    ///   (빈 목록 = 접근 없음, fail-closed — "unscoped = all"은 마스터/dev 전용)
    /// - `PerWorkspace`: 맵에 있으면 해당 권한, 없으면 None (fail-closed)
    pub fn permission_for(&self, workspace_id: &str) -> Option<Permission> {
        match &self.scope {
            WorkspaceScope::All => Some(Permission::Admin),
            WorkspaceScope::Fixed(list) => list
                .iter()
                .any(|w| w == workspace_id)
                .then(|| self.permissions.clone()),
            WorkspaceScope::PerWorkspace(map) => map.get(workspace_id).cloned(),
        }
    }

    /// 특정 워크스페이스에 접근 가능한지 확인 (`permission_for` 기반 단일 판정).
    pub fn can_access_workspace(&self, workspace_id: &str) -> bool {
        self.permission_for(workspace_id).is_some()
    }

    /// 특정 워크스페이스에 쓰기 가능한지 확인.
    pub fn can_write_workspace(&self, workspace_id: &str) -> bool {
        self.permission_for(workspace_id)
            .map_or(false, |p| p.can_write())
    }

    /// 특정 워크스페이스의 admin(owner급)인지 확인 — 멤버십·공개 설정 관리 판정.
    pub fn is_workspace_admin(&self, workspace_id: &str) -> bool {
        self.permission_for(workspace_id)
            .map_or(false, |p| p.is_admin())
    }

    /// 접근 가능한 워크스페이스 ID → 유효 권한 목록 (`/api/auth/me` 표시용).
    /// `All` 스코프는 전체 목록을 갖지 않으므로 호출측이 실제 워크스페이스
    /// 목록과 조합해야 한다 — 여기서는 명시적 스코프만 반환한다.
    pub fn scoped_workspaces(&self) -> Vec<(String, Permission)> {
        match &self.scope {
            WorkspaceScope::All => vec![],
            WorkspaceScope::Fixed(list) => list
                .iter()
                .map(|w| (w.clone(), self.permissions.clone()))
                .collect(),
            WorkspaceScope::PerWorkspace(map) => {
                map.iter().map(|(w, p)| (w.clone(), p.clone())).collect()
            }
        }
    }

    /// 워크스페이스 미지정 시 사용할 기본 워크스페이스 ID.
    ///
    /// - 마스터/개발모드/글로벌 admin(`All`): `default`
    /// - 시스템 키(`Fixed`): 목록의 첫 워크스페이스 (기존 의미 유지)
    /// - 유저/소유 키(`PerWorkspace`): 개인 워크스페이스 `u-{username}`가
    ///   접근 가능하면 그것, 아니면 정렬 순 첫 워크스페이스 (결정적)
    pub fn default_workspace(&self) -> String {
        match &self.scope {
            WorkspaceScope::All => "default".to_string(),
            WorkspaceScope::Fixed(list) => list
                .first()
                .cloned()
                .unwrap_or_else(|| "default".to_string()),
            WorkspaceScope::PerWorkspace(map) => {
                if let Some(user) = &self.user {
                    let personal = format!("u-{}", user.username);
                    if map.contains_key(&personal) {
                        return personal;
                    }
                }
                map.keys()
                    .next()
                    .cloned()
                    .unwrap_or_else(|| "default".to_string())
            }
        }
    }

    /// 전역(시스템) 쓰기 권한. 워크스페이스별 판정은 `can_write_workspace`를 쓸 것.
    pub fn can_write(&self) -> bool {
        self.permissions.can_write()
    }

    /// 전역(시스템) admin 여부 — 마스터키·admin 권한 키·글로벌 admin 계정.
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

/// `last_used_at` 디스크 반영 최소 간격(초).
///
/// 인증된 모든 요청이 요청당 파일 전체를 재작성하는 쓰기 증폭을 막기 위해, 이 간격
/// 안의 갱신은 디스크에 반영하지 않는다. `last_used_at`은 "안 쓰는 키 찾기"용 관측
/// 필드라 이 정도 granularity("최근 N초 내 사용")로 충분하다.
const LAST_USED_PERSIST_INTERVAL_SECS: i64 = 60;

/// API Key 관리자. `data/api_keys.json`에 키를 영속화하고 메모리 캐시를 유지한다.
pub struct ApiKeyManager {
    keys_path: PathBuf,
    keys: RwLock<Vec<ApiKey>>,
    /// 저장 직렬화 락. 동시 `save()`가 temp 파일/쓰기 순서를 침범하지 않도록 하고,
    /// 락 안에서 최신 인메모리 상태를 다시 읽어 폐기 직후 잔존 스냅샷 부활을 막는다.
    save_lock: tokio::sync::Mutex<()>,
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
            match serde_json::from_str::<Vec<ApiKey>>(&content) {
                Ok(keys) => keys,
                Err(e) => {
                    // 손상된 키 파일로 부팅을 막지 않는다: 손상본을 백업하고 빈 목록으로
                    // degrade한다(torn write/재배포 크래시 대비). 침묵 금지 — error로
                    // 명시하고, 마스터키(MAIA_API_KEY)로 복구할 수 있게 남긴다.
                    // (대조: SettingsManager::new도 unwrap_or_default로 graceful.)
                    let file_name = keys_path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("api_keys.json");
                    let backup = keys_path.with_file_name(format!("{}.corrupt", file_name));
                    if let Err(re) = fs::rename(&keys_path, &backup).await {
                        tracing::error!("손상된 api_keys.json 백업 실패: {}", re);
                    }
                    tracing::error!(
                        "api_keys.json 파싱 실패({}). 손상 파일을 {:?}로 백업하고 빈 키 \
                         목록으로 시작합니다. 마스터키(MAIA_API_KEY)로 접속해 키를 \
                         재발급하세요.",
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
            keys_path,
            keys: RwLock::new(keys),
            save_lock: tokio::sync::Mutex::new(()),
        })
    }

    /// 새 API Key를 생성한다. (ApiKey, 평문 키)를 반환한다.
    /// 평문 키는 이 시점에서만 확인 가능하며, 이후에는 해시만 저장된다.
    /// `owner`가 Some이면 계정 하위 키 — 유효 접근권이 계정 접근권과 교집합된다.
    pub async fn create_key(
        &self,
        label: String,
        workspaces: Vec<String>,
        permissions: Permission,
        expires_at: Option<DateTime<Utc>>,
        owner: Option<String>,
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
            owner,
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

    /// 평문 토큰을 인증한다.
    ///
    /// 토큰을 해시하여 조회하고(해시 비교 → 타이밍 공격 완화), 만료되지 않은
    /// 키만 반환한다. 일치하는 키가 없거나 만료되었으면 `None`.
    pub async fn authenticate(&self, raw_token: &str) -> Option<ApiKey> {
        let hashed = hash_key(raw_token);
        let key = self.find_by_hash(&hashed).await?;
        if key.is_expired() {
            tracing::warn!("Rejected expired API key: {}", key.key_id);
            return None;
        }
        Some(key)
    }

    /// 등록된 키가 있는지 확인한다.
    pub async fn has_keys(&self) -> bool {
        !self.keys.read().await.is_empty()
    }

    /// 특정 계정이 소유한 키 목록을 반환한다 (`/api/me/keys` 셀프서비스).
    pub async fn list_keys_for_owner(&self, user_id: &str) -> Vec<ApiKey> {
        let keys = self.keys.read().await;
        keys.iter()
            .filter(|k| k.owner.as_deref() == Some(user_id))
            .cloned()
            .collect()
    }

    /// 특정 계정이 소유한 키를 전부 폐기한다 (계정 삭제 연쇄 조치).
    /// 폐기된 키 수를 반환한다.
    pub async fn revoke_keys_for_owner(&self, user_id: &str) -> Result<usize> {
        let removed = {
            let mut keys = self.keys.write().await;
            let before = keys.len();
            keys.retain(|k| k.owner.as_deref() != Some(user_id));
            before - keys.len()
        };

        if removed > 0 {
            self.save().await?;
            tracing::info!("Revoked {} API key(s) owned by user: {}", removed, user_id);
        }
        Ok(removed)
    }

    /// `last_used_at`을 현재 시각으로 갱신한다.
    /// 미들웨어에서 `tokio::spawn`으로 비동기 호출하여 응답 지연을 방지한다.
    ///
    /// 요청당 전체 파일 재작성(쓰기 증폭)을 막기 위해, 마지막 반영이
    /// `LAST_USED_PERSIST_INTERVAL_SECS`보다 오래됐을 때만 갱신하고 디스크에 flush한다.
    pub async fn update_last_used(&self, key_id: &str) -> Result<()> {
        let should_save = {
            let mut keys = self.keys.write().await;
            match keys.iter_mut().find(|k| k.key_id == key_id) {
                Some(key) => {
                    let now = Utc::now();
                    let stale = key.last_used_at.map_or(true, |prev| {
                        now.signed_duration_since(prev).num_seconds()
                            >= LAST_USED_PERSIST_INTERVAL_SECS
                    });
                    if stale {
                        key.last_used_at = Some(now);
                    }
                    stale
                }
                None => false,
            }
        };

        if should_save {
            self.save().await?;
        }
        Ok(())
    }

    /// 현재 키 목록을 디스크에 원자적으로 영속화한다.
    ///
    /// temp 파일에 쓴 뒤 `rename`으로 교체해 torn write(쓰기 도중 크래시로 인한
    /// 잘린 파일 → 부팅 브릭)를 방지한다. `save_lock`으로 동시 저장을 직렬화하고,
    /// 락 안에서 최신 인메모리 상태를 다시 직렬화하므로 폐기 직후 잔존 스냅샷이
    /// 되살아나는 경합도 막는다.
    async fn save(&self) -> Result<()> {
        let _guard = self.save_lock.lock().await;

        let content = {
            let keys = self.keys.read().await;
            serde_json::to_string_pretty(&*keys)?
        };

        if let Some(parent) = self.keys_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        let file_name = self
            .keys_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("api_keys.json");
        let tmp_path = self.keys_path.with_file_name(format!("{}.tmp", file_name));

        fs::write(&tmp_path, content)
            .await
            .context("Failed to write api_keys.json.tmp")?;
        fs::rename(&tmp_path, &self.keys_path)
            .await
            .context("Failed to persist api_keys.json")?;

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
            owner: None,
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
    fn test_auth_context_empty_workspaces_is_fail_closed() {
        // 회귀 방지: 빈 workspaces 목록의 비마스터 키는 어떤 워크스페이스에도
        // 접근할 수 없어야 한다(fail-closed). 과거 `is_empty() → true` 단락이
        // 스코프 없는 키에 전 워크스페이스(개인정보 포함) 접근을 조용히 부여하는
        // 격리 우회였다. (Devil's Advocate Cycle 1 최우선 blocking 시나리오)
        let mut key = make_test_key();
        key.workspaces = vec![];
        let ctx = AuthContext::from_api_key(&key);
        assert!(!ctx.is_master);
        assert!(!ctx.can_access_workspace("default"));
        assert!(!ctx.can_access_workspace("personal"));
        assert!(!ctx.can_access_workspace("work"));
        assert!(!ctx.can_access_workspace(""));
    }

    #[test]
    fn test_can_access_and_has_access_agree_on_empty() {
        // 같은 개념을 판정하는 두 메서드는 빈 목록에서 일치해야 한다(둘 다 거부).
        // 과거 has_workspace_access(거부) vs can_access_workspace(전체허용)의
        // 정반대 판정이 구조적 결함의 근거였다.
        let mut key = make_test_key();
        key.workspaces = vec![];
        let ctx = AuthContext::from_api_key(&key);
        for ws in ["default", "personal", "work"] {
            assert_eq!(
                key.has_workspace_access(ws),
                ctx.can_access_workspace(ws),
                "has_workspace_access와 can_access_workspace는 '{}'에서 일치해야 한다",
                ws
            );
        }
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

    // ──── Permission::intersect ────

    #[test]
    fn test_permission_intersect_takes_lower() {
        use Permission::*;
        assert_eq!(ReadOnly.intersect(&Admin), ReadOnly);
        assert_eq!(Admin.intersect(&ReadOnly), ReadOnly);
        assert_eq!(ReadWrite.intersect(&Admin), ReadWrite);
        assert_eq!(Admin.intersect(&Admin), Admin);
        assert_eq!(ReadWrite.intersect(&ReadWrite), ReadWrite);
        assert_eq!(ReadOnly.intersect(&ReadWrite), ReadOnly);
    }

    // ──── WorkspaceScope — 유저/소유 키 컨텍스트 단위 판정 ────

    fn make_test_user(is_admin: bool) -> crate::auth::User {
        crate::auth::User {
            user_id: "user_test00000000".to_string(),
            username: "alice".to_string(),
            password_hash: "$argon2id$test".to_string(),
            display_name: "Alice".to_string(),
            is_admin,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn test_from_user_per_workspace_scope() {
        let mut access = BTreeMap::new();
        access.insert("ws-rw".to_string(), Permission::ReadWrite);
        access.insert("ws-ro".to_string(), Permission::ReadOnly);

        let ctx = AuthContext::from_user(&make_test_user(false), access);
        assert_eq!(ctx.source, AuthSource::Session);
        assert_eq!(ctx.permission_for("ws-rw"), Some(Permission::ReadWrite));
        assert_eq!(ctx.permission_for("ws-ro"), Some(Permission::ReadOnly));
        assert_eq!(ctx.permission_for("elsewhere"), None, "맵 밖은 fail-closed");
        assert!(ctx.can_write_workspace("ws-rw"));
        assert!(!ctx.can_write_workspace("ws-ro"));
        assert!(!ctx.is_admin(), "일반 계정은 전역 admin 아님");
        assert!(!ctx.is_master);
    }

    #[test]
    fn test_from_user_admin_gets_all_scope() {
        let ctx = AuthContext::from_user(&make_test_user(true), BTreeMap::new());
        assert!(ctx.is_admin());
        assert!(ctx.can_access_workspace("anything"));
        assert!(ctx.is_workspace_admin("anything"));
        assert!(!ctx.is_master, "글로벌 admin 계정은 마스터키와 구분된다");
    }

    #[test]
    fn test_default_workspace_prefers_personal() {
        // 유저 컨텍스트는 개인 워크스페이스 u-{username}를 기본으로 선호한다
        let mut access = BTreeMap::new();
        access.insert("a-first-sorted".to_string(), Permission::ReadWrite);
        access.insert("u-alice".to_string(), Permission::Admin);
        let ctx = AuthContext::from_user(&make_test_user(false), access);
        assert_eq!(ctx.default_workspace(), "u-alice");

        // 개인 ws가 접근 목록에 없으면 정렬 순 첫 워크스페이스 (결정적)
        let mut access = BTreeMap::new();
        access.insert("b-ws".to_string(), Permission::ReadWrite);
        access.insert("a-ws".to_string(), Permission::ReadOnly);
        let ctx = AuthContext::from_user(&make_test_user(false), access);
        assert_eq!(ctx.default_workspace(), "a-ws");

        // 접근이 하나도 없으면 default (resolve 단계에서 403으로 걸러진다)
        let ctx = AuthContext::from_user(&make_test_user(false), BTreeMap::new());
        assert_eq!(ctx.default_workspace(), "default");
    }

    #[test]
    fn test_from_owned_api_key_intersection_unit() {
        // 키 스코프 {a, c} ∩ 계정 접근 {a: RO, b: Admin} = {a: min(RW,RO)=RO}
        let mut key = make_test_key();
        key.workspaces = vec!["a".to_string(), "c".to_string()];
        key.permissions = Permission::ReadWrite;
        key.owner = Some("user_test00000000".to_string());

        let mut user_access = BTreeMap::new();
        user_access.insert("a".to_string(), Permission::ReadOnly);
        user_access.insert("b".to_string(), Permission::Admin);

        let ctx = AuthContext::from_owned_api_key(&key, &make_test_user(false), &user_access);
        assert_eq!(ctx.permission_for("a"), Some(Permission::ReadOnly));
        assert_eq!(ctx.permission_for("b"), None, "키 스코프 밖");
        assert_eq!(ctx.permission_for("c"), None, "계정 접근권 밖");
        assert!(!ctx.is_admin(), "일반 계정 소유 키는 전역 admin 불가");
        assert_eq!(ctx.user.as_ref().unwrap().username, "alice");
    }

    #[test]
    fn test_scoped_workspaces_listing() {
        // /api/auth/me 표시용 목록: Fixed는 (ws, 키 권한), PerWorkspace는 맵 그대로
        let key = make_test_key();
        let ctx = AuthContext::from_api_key(&key);
        let listed = ctx.scoped_workspaces();
        assert_eq!(listed.len(), 2);
        assert!(listed.contains(&("default".to_string(), Permission::ReadWrite)));

        assert!(AuthContext::master().scoped_workspaces().is_empty(), "All 스코프는 명시 목록 없음");
    }

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

        manager.create_key("Key 1".to_string(), vec!["default".to_string()], Permission::ReadOnly, None, None).await.unwrap();
        manager.create_key("Key 2".to_string(), vec!["work".to_string()], Permission::ReadWrite, None, None).await.unwrap();
        manager.create_key("Key 3".to_string(), vec!["default".to_string(), "work".to_string()], Permission::Admin, None, None).await.unwrap();

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

        manager.create_key("K".to_string(), vec![], Permission::ReadOnly, None, None).await.unwrap();
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
            None,
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
            None,
        ).await.unwrap();

        let hashed = hash_key(&raw);
        let key = manager.find_by_hash(&hashed).await.unwrap();

        assert!(key.has_workspace_access("allowed-ws"));
        assert!(!key.has_workspace_access("denied-ws"));
    }

    // ──── authenticate ────

    #[tokio::test]
    async fn test_authenticate_valid_key() {
        let (_tmp, manager) = setup_manager().await;
        let (key, raw) = manager.create_key(
            "Auth".to_string(),
            vec!["default".to_string()],
            Permission::ReadWrite,
            None,
            None,
        ).await.unwrap();

        let authed = manager.authenticate(&raw).await;
        assert!(authed.is_some());
        assert_eq!(authed.unwrap().key_id, key.key_id);
    }

    #[tokio::test]
    async fn test_authenticate_wrong_token() {
        let (_tmp, manager) = setup_manager().await;
        manager.create_key(
            "Auth".to_string(),
            vec!["default".to_string()],
            Permission::ReadWrite,
            None,
            None,
        ).await.unwrap();

        assert!(manager.authenticate("maia_wrongtoken").await.is_none());
    }

    #[tokio::test]
    async fn test_authenticate_expired_key_rejected() {
        let (_tmp, manager) = setup_manager().await;
        let expires = Utc::now() - Duration::seconds(5);
        let (_, raw) = manager.create_key(
            "Expired".to_string(),
            vec!["default".to_string()],
            Permission::ReadWrite,
            Some(expires),
            None,
        ).await.unwrap();

        // 만료된 키는 검색은 되지만 authenticate는 거부해야 한다
        assert!(manager.authenticate(&raw).await.is_none(), "만료 키는 인증되면 안 된다");
    }

    // ──── default_workspace ────

    #[test]
    fn test_default_workspace_master_is_default() {
        assert_eq!(AuthContext::master().default_workspace(), "default");
        assert_eq!(AuthContext::dev_mode().default_workspace(), "default");
    }

    #[test]
    fn test_default_workspace_bound_key_first() {
        let mut key = make_test_key();
        key.workspaces = vec!["work".to_string(), "personal".to_string()];
        let ctx = AuthContext::from_api_key(&key);
        assert_eq!(ctx.default_workspace(), "work");
    }

    #[test]
    fn test_default_workspace_empty_falls_back() {
        let mut key = make_test_key();
        key.workspaces = vec![];
        let ctx = AuthContext::from_api_key(&key);
        assert_eq!(ctx.default_workspace(), "default");
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
            None,
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

    // ──── 영속화 하드닝 (원자적 저장 / graceful degrade / 쓰기 증폭 억제) ────

    #[tokio::test]
    async fn test_manager_corrupt_file_degrades_gracefully() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("api_keys.json");
        // 손상된(파싱 불가) JSON 기록 — torn write 시뮬레이션
        fs::write(&path, "{ this is not valid json ]").await.unwrap();

        // 부팅이 막히지 않고 빈 목록으로 degrade해야 한다 (하드 실패 아님)
        let manager = ApiKeyManager::new(tmp.path().to_str().unwrap()).await.unwrap();
        assert!(manager.list_keys().await.is_empty());

        // 손상 파일은 .corrupt로 백업되어 보존된다 (복구 가능)
        let backup = tmp.path().join("api_keys.json.corrupt");
        assert!(backup.exists(), "손상 파일은 .corrupt로 백업되어야 한다");
    }

    #[tokio::test]
    async fn test_manager_save_is_atomic_no_temp_left() {
        let (tmp, manager) = setup_manager().await;
        manager.create_key("A".to_string(), vec!["default".to_string()], Permission::ReadOnly, None, None).await.unwrap();
        manager.create_key("B".to_string(), vec!["work".to_string()], Permission::ReadOnly, None, None).await.unwrap();

        // 원자적 저장(temp+rename) 후 .tmp 파일이 남지 않아야 한다
        let tmp_path = tmp.path().join("api_keys.json.tmp");
        assert!(!tmp_path.exists(), "저장 후 .tmp 파일이 남으면 안 된다");

        // 데이터는 온전히 재로드된다
        let m2 = ApiKeyManager::new(tmp.path().to_str().unwrap()).await.unwrap();
        assert_eq!(m2.list_keys().await.len(), 2);
    }

    #[tokio::test]
    async fn test_manager_update_last_used_coarsened() {
        let (_tmp, manager) = setup_manager().await;
        let (key, _) = manager.create_key(
            "Coarse".to_string(),
            vec!["default".to_string()],
            Permission::ReadOnly,
            None,
            None,
        ).await.unwrap();

        // 첫 갱신은 None → Some (임계값 무관하게 반영)
        manager.update_last_used(&key.key_id).await.unwrap();
        let first = manager.list_keys().await[0].last_used_at;
        assert!(first.is_some());

        // 임계값(60s) 안의 재갱신은 값을 바꾸지 않는다 (쓰기 증폭 억제)
        manager.update_last_used(&key.key_id).await.unwrap();
        let second = manager.list_keys().await[0].last_used_at;
        assert_eq!(first, second, "임계값 내 재갱신은 last_used_at을 변경하지 않아야 한다");
    }

    // ──── owner (계정 하위 키) ────

    #[test]
    fn test_api_key_deserialize_legacy_without_owner() {
        // owner 필드가 없는 기존 api_keys.json도 로드되어야 한다(하위호환).
        // 이 불변식이 깨지면 기동 시 기존 키 전체가 파싱 실패로 유실된다.
        let json = r#"{
            "key_id": "maia_sk_legacy000000",
            "hashed_key": "sha256:abc",
            "label": "Legacy",
            "workspaces": ["default"],
            "permissions": "read_write",
            "created_at": "2026-04-01T00:00:00Z",
            "last_used_at": null,
            "expires_at": null
        }"#;
        let key: ApiKey = serde_json::from_str(json).unwrap();
        assert_eq!(key.owner, None, "누락된 owner는 None(시스템 키)으로 기본값 처리");
    }

    #[tokio::test]
    async fn test_manager_list_keys_for_owner() {
        let (_tmp, manager) = setup_manager().await;
        manager.create_key("sys".to_string(), vec!["default".to_string()], Permission::ReadOnly, None, None).await.unwrap();
        manager.create_key("mine-1".to_string(), vec!["default".to_string()], Permission::ReadOnly, None, Some("user_a".to_string())).await.unwrap();
        manager.create_key("mine-2".to_string(), vec!["default".to_string()], Permission::ReadOnly, None, Some("user_a".to_string())).await.unwrap();
        manager.create_key("theirs".to_string(), vec!["default".to_string()], Permission::ReadOnly, None, Some("user_b".to_string())).await.unwrap();

        let mine = manager.list_keys_for_owner("user_a").await;
        assert_eq!(mine.len(), 2);
        assert!(mine.iter().all(|k| k.owner.as_deref() == Some("user_a")));
    }

    #[tokio::test]
    async fn test_manager_revoke_keys_for_owner() {
        let (tmp, manager) = setup_manager().await;
        manager.create_key("sys".to_string(), vec!["default".to_string()], Permission::ReadOnly, None, None).await.unwrap();
        let (_, raw_owned) = manager.create_key("mine".to_string(), vec!["default".to_string()], Permission::ReadOnly, None, Some("user_a".to_string())).await.unwrap();

        let removed = manager.revoke_keys_for_owner("user_a").await.unwrap();
        assert_eq!(removed, 1);
        assert!(manager.authenticate(&raw_owned).await.is_none(), "폐기된 소유 키는 인증 불가");
        assert_eq!(manager.list_keys().await.len(), 1, "시스템 키는 남는다");

        // 디스크에도 반영 (재로드 검증)
        let m2 = ApiKeyManager::new(tmp.path().to_str().unwrap()).await.unwrap();
        assert_eq!(m2.list_keys().await.len(), 1);

        // 소유 키가 없는 계정은 no-op
        assert_eq!(manager.revoke_keys_for_owner("user_none").await.unwrap(), 0);
    }
}
