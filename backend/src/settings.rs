use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

use crate::llm::{CodexAuth, CodexTokenStore, ProviderType};

/// 애플리케이션 설정
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Provider별 API Key
    pub api_keys: HashMap<ProviderType, String>,

    /// 파싱(요약/태그 추출)에 사용할 provider
    pub parsing_provider: ProviderType,

    /// 임베딩 생성에 사용할 provider
    pub embedding_provider: ProviderType,

    /// Codex(ChatGPT 구독 OAuth) 토큰 상태. 미임포트면 None.
    /// serde default라 기존 settings.json(codex 필드 없음)도 그대로 로드된다(하위호환).
    #[serde(default)]
    pub codex: Option<CodexAuth>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            api_keys: HashMap::new(),
            parsing_provider: ProviderType::Gemini,
            embedding_provider: ProviderType::Gemini,
            codex: None,
        }
    }
}

/// 설정 관리자
pub struct SettingsManager {
    settings: Arc<RwLock<Settings>>,
    file_path: PathBuf,
    /// Codex refresh 단일 플라이트 게이트. 모든 CodexProvider 인스턴스가 이 락을
    /// 공유해(SettingsManager는 Arc로 공유됨) 동시 파싱 호출의 중복 refresh를 막는다.
    codex_refresh_lock: Arc<Mutex<()>>,
}

impl SettingsManager {
    /// 새 설정 관리자 생성
    pub async fn new(data_dir: &str) -> Result<Self> {
        let file_path = PathBuf::from(data_dir).join("settings.json");

        let settings = if file_path.exists() {
            let content = tokio::fs::read_to_string(&file_path)
                .await
                .context("Failed to read settings file")?;
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            Settings::default()
        };

        Ok(Self {
            settings: Arc::new(RwLock::new(settings)),
            file_path,
            codex_refresh_lock: Arc::new(Mutex::new(())),
        })
    }

    /// 현재 설정 조회
    pub async fn get(&self) -> Settings {
        self.settings.read().await.clone()
    }

    /// 로컬 임베딩 모델 캐시 디렉토리(`DATA_DIR/models`).
    /// settings.json이 `DATA_DIR/settings.json`이므로 부모에서 유도한다.
    pub fn models_dir(&self) -> PathBuf {
        self.file_path
            .parent()
            .map(|p| p.join("models"))
            .unwrap_or_else(|| PathBuf::from("./data/models"))
    }

    /// Codex 토큰 상태 조회
    pub async fn get_codex_auth(&self) -> Option<CodexAuth> {
        self.settings.read().await.codex.clone()
    }

    /// Codex 토큰 상태 저장(임포트/refresh 회전 영속화)
    pub async fn set_codex_auth(&self, auth: CodexAuth) -> Result<()> {
        {
            let mut settings = self.settings.write().await;
            settings.codex = Some(auth);
        }
        self.save().await
    }

    /// Codex 토큰 상태 삭제
    pub async fn remove_codex_auth(&self) -> Result<()> {
        {
            let mut settings = self.settings.write().await;
            settings.codex = None;
        }
        self.save().await
    }

    /// API Key 설정
    pub async fn set_api_key(&self, provider: ProviderType, api_key: String) -> Result<()> {
        {
            let mut settings = self.settings.write().await;
            settings.api_keys.insert(provider, api_key);
        }
        self.save().await
    }

    /// API Key 조회
    pub async fn get_api_key(&self, provider: ProviderType) -> Option<String> {
        self.settings.read().await.api_keys.get(&provider).cloned()
    }

    /// API Key 삭제
    pub async fn remove_api_key(&self, provider: ProviderType) -> Result<()> {
        {
            let mut settings = self.settings.write().await;
            settings.api_keys.remove(&provider);
        }
        self.save().await
    }

    /// 파싱 provider 설정
    pub async fn set_parsing_provider(&self, provider: ProviderType) -> Result<()> {
        {
            let mut settings = self.settings.write().await;
            settings.parsing_provider = provider;
        }
        self.save().await
    }

    /// 임베딩 provider 설정
    pub async fn set_embedding_provider(&self, provider: ProviderType) -> Result<()> {
        {
            let mut settings = self.settings.write().await;
            settings.embedding_provider = provider;
        }
        self.save().await
    }

    /// 설정 전체 업데이트
    pub async fn update(&self, new_settings: Settings) -> Result<()> {
        {
            let mut settings = self.settings.write().await;
            *settings = new_settings;
        }
        self.save().await
    }

    /// 설정을 파일에 저장
    async fn save(&self) -> Result<()> {
        let settings = self.settings.read().await;
        let content = serde_json::to_string_pretty(&*settings)?;

        // 디렉토리가 없으면 생성
        if let Some(parent) = self.file_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        tokio::fs::write(&self.file_path, content)
            .await
            .context("Failed to write settings file")?;

        Ok(())
    }

    /// 등록된 provider 목록 (API Key 있는 것만)
    pub async fn available_providers(&self) -> Vec<ProviderType> {
        self.settings
            .read()
            .await
            .api_keys
            .keys()
            .cloned()
            .collect()
    }

    /// 특정 provider가 사용 가능한지 확인
    pub async fn is_provider_available(&self, provider: ProviderType) -> bool {
        self.settings
            .read()
            .await
            .api_keys
            .contains_key(&provider)
    }
}

/// Codex provider가 토큰 수명 관리에 쓰는 저장 계층. SettingsManager를 그대로
/// 재사용해, codex 토큰이 다른 provider 설정과 **동일한 settings.json**에 산다
/// (별도 파일 라이터 경합 없음 — 단일 진실 원천).
#[async_trait]
impl CodexTokenStore for SettingsManager {
    async fn load(&self) -> Option<CodexAuth> {
        self.get_codex_auth().await
    }

    async fn save(&self, auth: CodexAuth) -> Result<()> {
        self.set_codex_auth(auth).await
    }

    fn refresh_lock(&self) -> Arc<Mutex<()>> {
        self.codex_refresh_lock.clone()
    }
}

/// API 응답용 설정 (API Key는 마스킹)
#[derive(Debug, Serialize)]
pub struct SettingsResponse {
    /// 키 기반 provider(gemini/claude/openai)의 키 등록 상태.
    pub providers: Vec<ProviderInfo>,
    pub parsing_provider: ProviderType,
    pub embedding_provider: ProviderType,
    /// Codex(구독 OAuth) 상태 — 키가 아니라 auth.json 임포트 기반이라 별도 표현.
    pub codex: CodexStatus,
    /// 로컬 임베딩 상태 — 키 불요, 모델/차원/캐시 준비 여부.
    pub local: LocalStatus,
}

#[derive(Debug, Serialize)]
pub struct ProviderInfo {
    pub provider: ProviderType,
    pub has_api_key: bool,
    pub api_key_preview: Option<String>,
}

/// Codex 토큰 상태(시크릿 미노출 — 계정만 마스킹 프리뷰).
#[derive(Debug, Serialize)]
pub struct CodexStatus {
    pub has_auth: bool,
    pub account_preview: Option<String>,
    pub last_refresh: Option<chrono::DateTime<chrono::Utc>>,
}

/// 로컬 임베딩 상태.
#[derive(Debug, Serialize)]
pub struct LocalStatus {
    pub model: String,
    pub dim: usize,
    /// 모델 파일 캐시가 준비됐는지(다운로드 완료 여부의 근사).
    pub cache_ready: bool,
}

impl Settings {
    /// API 응답용으로 변환 (API Key 마스킹).
    ///
    /// `cache_ready`는 로컬 임베딩 모델 캐시 준비 여부로, 파일시스템 확인이 필요해
    /// (SettingsManager가 소유한 경로) 호출 측에서 계산해 주입한다.
    pub fn to_response(&self, cache_ready: bool) -> SettingsResponse {
        let providers = vec![
            ProviderType::Gemini,
            ProviderType::Claude,
            ProviderType::OpenAi,
        ]
        .into_iter()
        .map(|provider| {
            let api_key = self.api_keys.get(&provider);
            ProviderInfo {
                provider,
                has_api_key: api_key.is_some(),
                api_key_preview: api_key.map(|k: &String| {
                    if k.len() > 8 {
                        format!("{}...{}", &k[..4], &k[k.len() - 4..])
                    } else {
                        "****".to_string()
                    }
                }),
            }
        })
        .collect();

        let codex = CodexStatus {
            has_auth: self.codex.is_some(),
            account_preview: self.codex.as_ref().map(|c| c.account_preview()),
            last_refresh: self.codex.as_ref().and_then(|c| c.last_refresh),
        };

        let local = LocalStatus {
            model: crate::llm::LOCAL_EMBED_MODEL.to_string(),
            dim: crate::llm::LOCAL_EMBED_DIM,
            cache_ready,
        };

        SettingsResponse {
            providers,
            parsing_provider: self.parsing_provider,
            embedding_provider: self.embedding_provider,
            codex,
            local,
        }
    }
}
