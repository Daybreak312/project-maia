use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::llm::ProviderType;

/// 애플리케이션 설정
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Provider별 API Key
    pub api_keys: HashMap<ProviderType, String>,

    /// 파싱(요약/태그 추출)에 사용할 provider
    pub parsing_provider: ProviderType,

    /// 임베딩 생성에 사용할 provider
    pub embedding_provider: ProviderType,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            api_keys: HashMap::new(),
            parsing_provider: ProviderType::Gemini,
            embedding_provider: ProviderType::Gemini,
        }
    }
}

/// 설정 관리자
pub struct SettingsManager {
    settings: Arc<RwLock<Settings>>,
    file_path: PathBuf,
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
        })
    }

    /// 현재 설정 조회
    pub async fn get(&self) -> Settings {
        self.settings.read().await.clone()
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

/// API 응답용 설정 (API Key는 마스킹)
#[derive(Debug, Serialize)]
pub struct SettingsResponse {
    pub providers: Vec<ProviderInfo>,
    pub parsing_provider: ProviderType,
    pub embedding_provider: ProviderType,
}

#[derive(Debug, Serialize)]
pub struct ProviderInfo {
    pub provider: ProviderType,
    pub has_api_key: bool,
    pub api_key_preview: Option<String>,
}

impl Settings {
    /// API 응답용으로 변환 (API Key 마스킹)
    pub fn to_response(&self) -> SettingsResponse {
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

        SettingsResponse {
            providers,
            parsing_provider: self.parsing_provider,
            embedding_provider: self.embedding_provider,
        }
    }
}
