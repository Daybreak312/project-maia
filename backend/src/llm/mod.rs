mod gemini;
mod claude;
mod openai;

pub use gemini::GeminiProvider;
pub use claude::ClaudeProvider;
pub use openai::{OpenAiChatProvider, OpenAiEmbeddingProvider};

use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::models::ParsedContent;

/// 외부 LLM/임베딩 HTTP 요청의 전체 타임아웃(초).
///
/// reqwest 기본값은 **타임아웃 없음**이라, 프로바이더가 연결을 열어둔 채 무응답하면
/// `parse`/`complete`/`embed` future가 영원히 pending → ingest 요청이 무한 hang된다.
/// 이 상한이 있어야 지연이 `Err`로 귀결되고, 상위의 raw 폴백 경로(정보 유실 0)가
/// 비로소 발동한다(PRD 인수 조건 "타임아웃 시 폴백").
///
/// 값은 thinking 계열 모델(gemini-2.5-flash)이 10KB+ 문서를 파싱할 때 60초를
/// 초과하는 실측(2026-07-07, memory/2026-07-07.md 13KB가 60초 타임아웃으로
/// 결정적 실패)에 맞춰 300초로 둔다. 무한 hang 방지 불변식은 그대로 유지된다.
const HTTP_TIMEOUT_SECS: u64 = 300;

/// 연결 수립 타임아웃(초). 도달 불가 호스트에서 빠르게 실패하도록 한다.
const HTTP_CONNECT_TIMEOUT_SECS: u64 = 10;

/// 타임아웃이 설정된 공용 reqwest 클라이언트를 만든다(모든 provider 공용).
///
/// 모든 provider가 이 한 곳을 거쳐 클라이언트를 만들어, "무한 hang 방지"라는
/// 안전 불변식이 단일 지점에서 보장된다.
pub fn build_http_client() -> Client {
    build_http_client_with(
        Duration::from_secs(HTTP_TIMEOUT_SECS),
        Duration::from_secs(HTTP_CONNECT_TIMEOUT_SECS),
    )
}

/// 타임아웃을 파라미터로 받는 내부 빌더(테스트에서 짧은 타임아웃으로 검증한다).
/// 빌더가 실패하는 극단적 상황에서만 무타임아웃 기본 클라이언트로 폴백한다.
fn build_http_client_with(timeout: Duration, connect_timeout: Duration) -> Client {
    Client::builder()
        .timeout(timeout)
        .connect_timeout(connect_timeout)
        .build()
        .unwrap_or_else(|_| Client::new())
}

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

    /// 자유 형식 프롬프트에 대한 원문 텍스트 응답을 생성한다 (에이전트 판단용).
    ///
    /// `parse`가 고정 스키마(summary/entities/facts)를 강제하는 것과 달리,
    /// `complete`는 임의 프롬프트에 대한 응답 문자열을 그대로 반환한다. 호출
    /// 측(IngestAgent 등)이 응답을 구조화 파싱하고, 실패 시 폴백을 책임진다.
    /// 이 메서드는 `LlmProvider`를 mock으로 대체해 에이전트 로직을 테스트하는
    /// 확장점이기도 하다.
    async fn complete(&self, prompt: &str) -> Result<String>;

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
  "entities": [
    {{
      "entity_type": "company|person|money|date|skill|project|location|other",
      "value": "원본에 명시된 값 그대로",
      "context": "원본에서 해당 값이 언급된 맥락"
    }}
  ],
  "facts": [
    "원문에서 추출한 독립적 사실 문장"
  ]
}}

## 추출 규칙
- summary: 원본 키워드 보존, 나중에 검색될 수 있도록 핵심어 포함
- entities: 회사명, 금액, 날짜, 기술스택 등 구체적 고유명사/값
- facts: 원문을 독립적으로 이해 가능한 사실 문장(atomic fact)으로 분해. 각 문장은 주어/목적어를 생략하지 않고 단독으로 의미가 통해야 함. 원문이 1-2문장으로 짧으면 빈 배열 허용
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_http_client_ok() {
        // 공용 클라이언트가 패닉/실패 없이 생성되어야 한다(타임아웃 설정 경로).
        let _client = build_http_client();
    }

    /// 무응답(블랙홀) 서버에 대한 요청이 타임아웃으로 실패하는지 검증한다.
    ///
    /// reqwest 기본 `Client::new()`는 타임아웃이 없어 이 상황에서 영원히 hang된다.
    /// 이 테스트는 `build_http_client_with`가 실제로 타임아웃을 걸어, 지연이 `Err`로
    /// 귀결됨을 고정한다 — 상위 raw 폴백이 발동할 수 있는 전제 조건이다.
    /// 루프백(127.0.0.1)만 사용하므로 외부 의존이 없고 결정적이다.
    #[tokio::test]
    async fn test_http_client_times_out_on_unresponsive_server() {
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // 연결은 수락하되 응답을 절대 보내지 않는 블랙홀 서버. 수락한 소켓을 붙들어
        // 연결이 닫히지 않게 유지한다(닫히면 타임아웃이 아닌 다른 에러가 난다).
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                held.push(stream);
            }
        });

        let client =
            build_http_client_with(Duration::from_millis(200), Duration::from_millis(200));
        let start = std::time::Instant::now();
        let result = client.get(format!("http://{addr}/")).send().await;

        assert!(result.is_err(), "무응답 서버 요청은 타임아웃으로 실패해야 한다");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "타임아웃이 상한 내에 발동해야 한다(무한 hang 금지)"
        );
    }
}
