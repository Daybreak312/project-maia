mod gemini;
mod claude;
mod openai;
mod codex;
mod local;

pub use gemini::GeminiProvider;
pub use claude::ClaudeProvider;
pub use openai::{OpenAiChatProvider, OpenAiEmbeddingProvider};
pub use codex::{CodexAuth, CodexProvider, CodexTokenStore, parse_codex_auth_json};
pub use local::{LocalEmbeddingProvider, LOCAL_EMBED_DIM, LOCAL_EMBED_MODEL};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::models::{Entity, ParsedContent};

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
///
/// - `Gemini`/`Claude`/`OpenAi`: 파싱+임베딩(claude 제외) 가능한 종량제/구독 키 provider.
/// - `Codex`: ChatGPT 구독 OAuth 기반 **파싱 전용** provider(임베딩 불가).
/// - `Local`: 외부 호출 없는 로컬 **임베딩 전용** provider(파싱 불가).
///
/// 파싱/임베딩 교차 선택 제약은 [`ProviderType::valid_for_parsing`]/
/// [`ProviderType::valid_for_embedding`]에서 단일 지점으로 판정한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderType {
    Gemini,
    Claude,
    OpenAi,
    Codex,
    Local,
}

impl std::fmt::Display for ProviderType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderType::Gemini => write!(f, "gemini"),
            ProviderType::Claude => write!(f, "claude"),
            ProviderType::OpenAi => write!(f, "openai"),
            ProviderType::Codex => write!(f, "codex"),
            ProviderType::Local => write!(f, "local"),
        }
    }
}

impl ProviderType {
    /// 파싱 provider로 선택 가능한가. `local`은 임베딩 전용이라 불가.
    pub fn valid_for_parsing(&self) -> bool {
        !matches!(self, ProviderType::Local)
    }

    /// 임베딩 provider로 선택 가능한가. `codex`는 파싱 전용, `claude`는 임베딩 미지원.
    pub fn valid_for_embedding(&self) -> bool {
        matches!(
            self,
            ProviderType::Gemini | ProviderType::OpenAi | ProviderType::Local
        )
    }

    /// 사용자가 키/토큰을 직접 등록하는 provider인가.
    /// `codex`는 auth.json 임포트, `local`은 키 불요라 별도 경로를 쓴다.
    pub fn uses_api_key(&self) -> bool {
        matches!(
            self,
            ProviderType::Gemini | ProviderType::Claude | ProviderType::OpenAi
        )
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

    /// **문서(passage)** 텍스트를 벡터로 변환한다.
    ///
    /// 문서 청크(summary/fact) 인덱싱에 쓰인다. e5 계열처럼 문서/쿼리 비대칭
    /// 임베딩을 요구하는 모델은 이 경로에 `passage: ` 접두를 붙인다.
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// **쿼리** 텍스트를 벡터로 변환한다.
    ///
    /// 기본 구현은 문서 임베딩과 동일하다(gemini/openai는 문서/쿼리를 구분하지
    /// 않는다). e5 계열은 이 경로에 `query: ` 접두를 붙이도록 오버라이드한다.
    /// 검색·후보 탐색 경로가 문서 인덱싱과 대칭이 되도록 별도 진입점을 둔다.
    async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.embed(text).await
    }

    /// 벡터 차원 수 반환
    fn dimension(&self) -> usize;

    /// API Key 유효성 검증
    async fn validate_api_key(&self) -> Result<bool>;
}

/// 키 기반 LLM Provider 생성 팩토리.
///
/// `codex`/`local`은 단순 키가 아니라 토큰 저장소·data_dir 컨텍스트가 필요하므로
/// 이 팩토리로 만들지 않는다(호출 측 `Indexer::llm_provider_for`가 분기). 그
/// 계약을 명시적 에러로 못박아, 잘못된 경로로 생성되는 것을 컴파일이 아닌
/// 런타임에서라도 분명히 드러낸다(침묵 실패 금지).
pub fn create_llm_provider(
    provider_type: ProviderType,
    api_key: String,
) -> Result<Box<dyn LlmProvider>> {
    match provider_type {
        ProviderType::Gemini => Ok(Box::new(GeminiProvider::new(api_key))),
        ProviderType::Claude => Ok(Box::new(ClaudeProvider::new(api_key))),
        ProviderType::OpenAi => Ok(Box::new(OpenAiChatProvider::new(api_key))),
        ProviderType::Codex => Err(anyhow!(
            "codex는 토큰 저장소 컨텍스트가 필요합니다 (Indexer::llm_provider_for 경유)"
        )),
        ProviderType::Local => Err(anyhow!("local은 임베딩 전용입니다 (파싱 provider로 사용 불가)")),
    }
}

/// 키 기반 Embedding Provider 생성 팩토리.
///
/// `local`은 data_dir 컨텍스트가 필요하므로 이 팩토리 밖(`Indexer`)에서 생성한다.
/// `claude`/`codex`는 임베딩을 지원하지 않아 명시적 에러를 반환한다.
pub fn create_embedding_provider(
    provider_type: ProviderType,
    api_key: String,
) -> Result<Box<dyn EmbeddingProvider>> {
    match provider_type {
        ProviderType::Gemini => Ok(Box::new(gemini::GeminiEmbeddingProvider::new(api_key))),
        ProviderType::OpenAi => Ok(Box::new(OpenAiEmbeddingProvider::new(api_key))),
        ProviderType::Claude => Err(anyhow!("claude는 임베딩을 지원하지 않습니다")),
        ProviderType::Codex => Err(anyhow!("codex는 임베딩 provider로 사용할 수 없습니다")),
        ProviderType::Local => Err(anyhow!(
            "local은 data_dir 컨텍스트가 필요합니다 (Indexer::embedding_provider_for 경유)"
        )),
    }
}

/// LLM 원문 응답(JSON)을 [`ParsedContent`]로 변환하는 공용 파서.
///
/// 코드블록 제거 → JSON 파싱 → entity 타입 매핑을 한 곳에 모아, provider별로
/// 중복되던 `ParsedContentRaw` 매핑 로직을 통일한다(신규 codex provider가 재사용).
pub fn parse_llm_json(text: &str) -> Result<ParsedContent> {
    let json_str = extract_json(text);
    let parsed: ParsedContentRaw =
        serde_json::from_str(json_str).map_err(|e| anyhow!("LLM 응답 JSON 파싱 실패: {e}"))?;
    Ok(ParsedContent {
        summary: parsed.summary,
        entities: parsed
            .entities
            .into_iter()
            .map(|e| Entity {
                entity_type: parse_entity_type(&e.entity_type),
                value: e.value,
                context: e.context,
            })
            .collect(),
        facts: parsed.facts,
    })
}

/// provider 공용 파싱 응답 스키마 (JSON 역직렬화용).
#[derive(Debug, Deserialize)]
struct ParsedContentRaw {
    summary: String,
    #[serde(default)]
    entities: Vec<EntityRaw>,
    #[serde(default)]
    facts: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct EntityRaw {
    entity_type: String,
    value: String,
    context: Option<String>,
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

    // ──── provider 교차 선택 제약 (FR5 validation 단일 지점) ────

    #[test]
    fn test_valid_for_parsing() {
        // local은 임베딩 전용 → 파싱 불가. 나머지는 파싱 가능.
        assert!(ProviderType::Gemini.valid_for_parsing());
        assert!(ProviderType::Claude.valid_for_parsing());
        assert!(ProviderType::OpenAi.valid_for_parsing());
        assert!(ProviderType::Codex.valid_for_parsing());
        assert!(!ProviderType::Local.valid_for_parsing());
    }

    #[test]
    fn test_valid_for_embedding() {
        // codex는 파싱 전용, claude는 임베딩 미지원 → 임베딩 불가.
        assert!(ProviderType::Gemini.valid_for_embedding());
        assert!(ProviderType::OpenAi.valid_for_embedding());
        assert!(ProviderType::Local.valid_for_embedding());
        assert!(!ProviderType::Codex.valid_for_embedding());
        assert!(!ProviderType::Claude.valid_for_embedding());
    }

    #[test]
    fn test_uses_api_key() {
        // 키 등록 경로를 쓰는 provider만 true. codex(import)/local(불요)은 false.
        assert!(ProviderType::Gemini.uses_api_key());
        assert!(ProviderType::Claude.uses_api_key());
        assert!(ProviderType::OpenAi.uses_api_key());
        assert!(!ProviderType::Codex.uses_api_key());
        assert!(!ProviderType::Local.uses_api_key());
    }

    #[test]
    fn test_provider_display_and_serde() {
        // Display와 serde(rename lowercase)가 일치해야 프론트/설정 왕복이 안전하다.
        assert_eq!(ProviderType::Codex.to_string(), "codex");
        assert_eq!(ProviderType::Local.to_string(), "local");
        assert_eq!(
            serde_json::to_string(&ProviderType::Codex).unwrap(),
            "\"codex\""
        );
        assert_eq!(
            serde_json::from_str::<ProviderType>("\"local\"").unwrap(),
            ProviderType::Local
        );
    }

    #[test]
    fn test_key_based_factories_reject_codex_local() {
        // codex/local은 키 팩토리로 만들 수 없다(전용 경로 강제 — 침묵 실패 금지).
        assert!(create_llm_provider(ProviderType::Codex, "k".into()).is_err());
        assert!(create_llm_provider(ProviderType::Local, "k".into()).is_err());
        assert!(create_embedding_provider(ProviderType::Codex, "k".into()).is_err());
        assert!(create_embedding_provider(ProviderType::Local, "k".into()).is_err());
        // claude 임베딩은 미지원.
        assert!(create_embedding_provider(ProviderType::Claude, "k".into()).is_err());
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
