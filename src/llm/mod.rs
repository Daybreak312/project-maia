mod gemini;
mod claude;
mod openai;

pub use gemini::GeminiProvider;
pub use claude::ClaudeProvider;
pub use openai::{OpenAiChatProvider, OpenAiEmbeddingProvider};

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::models::{Entity, ParsedContent};

/// LLM Provider 종류
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderType {
    Gemini,
    Claude,
    OpenAi,
}

impl std::fmt::Display for ProviderType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderType::Gemini => write!(f, "gemini"),
            ProviderType::Claude => write!(f, "claude"),
            ProviderType::OpenAi => write!(f, "openai"),
        }
    }
}

/// 자연어 파싱용 LLM Provider
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Provider 타입 반환
    fn provider_type(&self) -> ProviderType;

    /// 자연어 텍스트를 파싱하여 구조화된 데이터 반환
    async fn parse(&self, content: &str) -> Result<ParsedContent>;

    /// API Key 유효성 검증
    async fn validate_api_key(&self) -> Result<bool>;
}

/// 임베딩 생성용 Provider
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Provider 타입 반환
    fn provider_type(&self) -> ProviderType;

    /// 텍스트를 벡터로 변환
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// 벡터 차원 수 반환
    fn dimension(&self) -> usize;

    /// API Key 유효성 검증
    async fn validate_api_key(&self) -> Result<bool>;
}

/// LLM Provider 생성 팩토리
pub fn create_llm_provider(provider_type: ProviderType, api_key: String) -> Box<dyn LlmProvider> {
    match provider_type {
        ProviderType::Gemini => Box::new(GeminiProvider::new(api_key)),
        ProviderType::Claude => Box::new(ClaudeProvider::new(api_key)),
        ProviderType::OpenAi => Box::new(OpenAiChatProvider::new(api_key)),
    }
}

/// Embedding Provider 생성 팩토리
pub fn create_embedding_provider(provider_type: ProviderType, api_key: String) -> Box<dyn EmbeddingProvider> {
    match provider_type {
        ProviderType::Gemini => Box::new(gemini::GeminiEmbeddingProvider::new(api_key)),
        ProviderType::OpenAi => Box::new(OpenAiEmbeddingProvider::new(api_key)),
        // Claude는 임베딩 미지원, OpenAI로 폴백
        ProviderType::Claude => Box::new(OpenAiEmbeddingProvider::new(api_key)),
    }
}

/// 파싱 프롬프트 생성 (모든 provider 공용)
pub fn build_parse_prompt(content: &str) -> String {
    format!(
        r#"당신은 개인 메모/기록 저장 시스템의 파서입니다.
사용자가 입력한 텍스트를 나중에 검색할 수 있도록 구조화합니다.

## 핵심 원칙
- **절대로 원본에 없는 내용을 추가하거나 추론하지 마세요**
- 원본 텍스트에 명시적으로 언급된 정보만 추출하세요
- 해석, 판단, 감정 추론을 하지 마세요

## 입력 텍스트
"""
{content}
"""

## 출력 형식 (JSON)
{{
  "summary": "원본의 핵심 내용을 1-2문장으로 압축. 원본에 있는 키워드를 그대로 사용",
  "tags": ["원본에서 파생된 분류 태그 3-5개"],
  "entities": [
    {{
      "entity_type": "company|person|money|date|skill|project|location|other",
      "value": "원본에 명시된 값 그대로",
      "context": "원본에서 해당 값이 언급된 맥락"
    }}
  ]
}}

## 추출 규칙
- summary: 원본 키워드 보존, 나중에 검색될 수 있도록 핵심어 포함
- tags: 면접, 연봉협상, 이직 등 일반적 분류
- entities: 회사명, 금액, 날짜, 기술스택 등 구체적 고유명사/값
- JSON만 출력, 다른 텍스트 없이"#
    )
}

/// JSON 응답에서 코드블록 제거
pub fn extract_json(text: &str) -> &str {
    text.trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim()
}

/// entity_type 문자열을 EntityType으로 변환
pub fn parse_entity_type(s: &str) -> crate::models::EntityType {
    use crate::models::EntityType;

    match s.to_lowercase().as_str() {
        "company" => EntityType::Company,
        "person" => EntityType::Person,
        "money" => EntityType::Money,
        "date" => EntityType::Date,
        "skill" => EntityType::Skill,
        "project" => EntityType::Project,
        "location" => EntityType::Location,
        _ => EntityType::Other(s.to_string()),
    }
}
