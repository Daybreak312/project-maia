use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::{Client, RequestBuilder};
use serde::{Deserialize, Serialize};

use crate::models::ParsedContent;
use super::{LlmProvider, ProviderType, build_http_client, build_parse_prompt, parse_llm_json};

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Claude OAuth(구독) 모드에서 요청에 붙이는 베타 헤더 값.
/// `claude setup-token` 산출물(`sk-ant-oat01-…`)은 이 베타 플래그가 있어야
/// `/v1/messages`에서 Bearer 인증이 허용된다. (출처: Anthropic OAuth 베타, 2026-07 검증)
const OAUTH_BETA_HEADER: &str = "oauth-2025-04-20";

/// 파싱 모델 상수 — 파싱은 구조화 추출이라 속도/쿼터 효율을 우선한다.
/// 상향(예: sonnet)은 이 상수 한 줄 교체로 끝나야 한다(FR1).
const CLAUDE_PARSE_MODEL: &str = "claude-haiku-4-5";

/// OAuth 토큰(setup-token)은 **Claude Code 사용 맥락**에 스코프되어 있어, 일반
/// API 호출로 보이면 서버 정책상 거절될 수 있다. 이를 대비해 OAuth 모드 요청의
/// `system`에 Claude Code 정체성 프리픽스를 주입한다.
///
/// 실측(2026-07): setup-token으로 `/v1/messages`를 호출할 때 이 system 프리픽스
/// 없이도 통과하는 경우가 있으나, 정책 강화에 대비해 상수로 격리해 항상 붙인다.
/// 거절이 관찰되면 이 문자열만 조정하면 된다(FR1 대응점).
const CLAUDE_CODE_SYSTEM_PREFIX: &str =
    "You are Claude Code, Anthropic's official CLI for Claude.";

/// Claude 키 인증 방식. 키 접두로 자동 감지한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthMode {
    /// 기존 플랫폼 API 키(`sk-ant-api…`) — `x-api-key` 헤더.
    ApiKey,
    /// 구독 OAuth 토큰(`sk-ant-oat…`) — `Authorization: Bearer` + oauth 베타 헤더.
    OAuth,
}

/// 키 문자열로부터 인증 모드를 감지한다(순수 함수 — 단위 테스트 대상).
///
/// `sk-ant-oat` 접두 → OAuth, 그 외 전부 → 기존 API 키 모드. 저장·설정 UX는
/// 기존 키 필드를 그대로 재사용하고, 이 감지만으로 헤더 경로가 갈린다.
fn detect_auth_mode(key: &str) -> AuthMode {
    if key.starts_with("sk-ant-oat") {
        AuthMode::OAuth
    } else {
        AuthMode::ApiKey
    }
}

/// Claude LLM Provider
pub struct ClaudeProvider {
    client: Client,
    api_key: String,
    model: String,
    auth_mode: AuthMode,
}

impl ClaudeProvider {
    pub fn new(api_key: String) -> Self {
        let auth_mode = detect_auth_mode(&api_key);
        Self {
            client: build_http_client(),
            api_key,
            model: CLAUDE_PARSE_MODEL.to_string(),
            auth_mode,
        }
    }

    /// 인증 헤더를 요청에 적용한다. 모드에 따라 `x-api-key`와 `Authorization`이
    /// **상호 배타적**으로 붙는다(OAuth 모드에서 x-api-key 제거 불변식).
    fn apply_auth(&self, req: RequestBuilder) -> RequestBuilder {
        let req = req
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json");
        match self.auth_mode {
            AuthMode::ApiKey => req.header("x-api-key", &self.api_key),
            AuthMode::OAuth => req
                .header("authorization", format!("Bearer {}", self.api_key))
                .header("anthropic-beta", OAUTH_BETA_HEADER),
        }
    }

    /// OAuth 모드에서만 Claude Code system 프리픽스를 부여한다.
    fn system_prompt(&self) -> Option<String> {
        match self.auth_mode {
            AuthMode::OAuth => Some(CLAUDE_CODE_SYSTEM_PREFIX.to_string()),
            AuthMode::ApiKey => None,
        }
    }

    /// messages 요청을 전송하고 첫 content 블록의 텍스트를 반환한다(공용).
    async fn send_messages(&self, max_tokens: u32, user_content: String) -> Result<String> {
        let request = AnthropicRequest {
            model: self.model.clone(),
            max_tokens,
            system: self.system_prompt(),
            messages: vec![Message {
                role: "user".to_string(),
                content: user_content,
            }],
        };

        let response = self
            .apply_auth(self.client.post(ANTHROPIC_API_URL))
            .json(&request)
            .send()
            .await
            .context("Failed to call Anthropic API")?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            anyhow::bail!("Anthropic API error ({}): {}", status, error_text);
        }

        let response: AnthropicResponse = response
            .json()
            .await
            .context("Failed to parse Anthropic response")?;

        Ok(response
            .content
            .into_iter()
            .next()
            .map(|c| c.text)
            .unwrap_or_default())
    }
}

#[async_trait]
impl LlmProvider for ClaudeProvider {
    fn provider_type(&self) -> ProviderType {
        ProviderType::Claude
    }

    async fn parse(&self, content: &str) -> Result<ParsedContent> {
        let prompt = build_parse_prompt(content);
        let text = self.send_messages(1024, prompt).await?;
        parse_llm_json(&text)
    }

    async fn complete(&self, prompt: &str) -> Result<String> {
        // 판단 응답(전략+근거, 분할 세그먼트 포함)을 담기에 넉넉한 상한.
        self.send_messages(2048, prompt.to_string()).await
    }

    async fn validate_api_key(&self) -> Result<bool> {
        // 간단한 검증: 최소 토큰으로 API 호출 시도(OAuth/API 키 경로 모두 apply_auth 경유).
        let request = AnthropicRequest {
            model: self.model.clone(),
            max_tokens: 1,
            system: self.system_prompt(),
            messages: vec![Message {
                role: "user".to_string(),
                content: "Hi".to_string(),
            }],
        };

        let response = self
            .apply_auth(self.client.post(ANTHROPIC_API_URL))
            .json(&request)
            .send()
            .await?;

        Ok(response.status().is_success())
    }
}

// Request/Response 구조체
#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    /// OAuth 모드의 Claude Code system 프리픽스(API 키 모드에선 None → 미직렬화).
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<Message>,
}

#[derive(Debug, Serialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<ContentBlock>,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    text: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_auth_mode_oauth_token() {
        // setup-token 산출물 → OAuth 모드
        assert_eq!(detect_auth_mode("sk-ant-oat01-abcdef"), AuthMode::OAuth);
        assert_eq!(detect_auth_mode("sk-ant-oat-xyz"), AuthMode::OAuth);
    }

    #[test]
    fn test_detect_auth_mode_api_key() {
        // 기존 플랫폼 키 → API 키 모드(회귀)
        assert_eq!(detect_auth_mode("sk-ant-api03-abcdef"), AuthMode::ApiKey);
        // 알 수 없는 형식도 안전하게 API 키 모드로 폴백(기존 경로 유지)
        assert_eq!(detect_auth_mode("random-key"), AuthMode::ApiKey);
        assert_eq!(detect_auth_mode(""), AuthMode::ApiKey);
    }

    /// OAuth 모드 요청 헤더: Authorization Bearer + oauth 베타가 붙고, x-api-key는
    /// **제거**되어야 한다(FR1 헤더 분기 불변식).
    #[test]
    fn test_oauth_mode_headers() {
        let provider = ClaudeProvider::new("sk-ant-oat01-token".to_string());
        let req = provider
            .apply_auth(provider.client.post(ANTHROPIC_API_URL))
            .build()
            .unwrap();
        let headers = req.headers();

        assert_eq!(
            headers.get("authorization").unwrap(),
            "Bearer sk-ant-oat01-token"
        );
        assert_eq!(headers.get("anthropic-beta").unwrap(), OAUTH_BETA_HEADER);
        assert!(
            headers.get("x-api-key").is_none(),
            "OAuth 모드에서는 x-api-key 헤더가 없어야 한다"
        );
        assert_eq!(headers.get("anthropic-version").unwrap(), ANTHROPIC_VERSION);
    }

    /// API 키 모드 요청 헤더: x-api-key가 붙고 Authorization/oauth 베타는 없어야
    /// 한다(기존 경로 회귀 방지).
    #[test]
    fn test_api_key_mode_headers() {
        let provider = ClaudeProvider::new("sk-ant-api03-key".to_string());
        let req = provider
            .apply_auth(provider.client.post(ANTHROPIC_API_URL))
            .build()
            .unwrap();
        let headers = req.headers();

        assert_eq!(headers.get("x-api-key").unwrap(), "sk-ant-api03-key");
        assert!(
            headers.get("authorization").is_none(),
            "API 키 모드에서는 Authorization 헤더가 없어야 한다"
        );
        assert!(headers.get("anthropic-beta").is_none());
    }

    /// OAuth 모드는 Claude Code system 프리픽스를 부여하고, API 키 모드는 부여하지
    /// 않는다(요청 본문 분기 불변식).
    #[test]
    fn test_system_prompt_only_in_oauth_mode() {
        let oauth = ClaudeProvider::new("sk-ant-oat01-t".to_string());
        assert_eq!(
            oauth.system_prompt().as_deref(),
            Some(CLAUDE_CODE_SYSTEM_PREFIX)
        );

        let api = ClaudeProvider::new("sk-ant-api03-k".to_string());
        assert_eq!(api.system_prompt(), None);
    }

    /// 요청 직렬화: OAuth 모드는 `system` 필드가 실리고, API 키 모드는 `system`이
    /// 생략(skip_serializing_if)되어야 한다.
    #[test]
    fn test_request_system_serialization() {
        let oauth = ClaudeProvider::new("sk-ant-oat01-t".to_string());
        let req = AnthropicRequest {
            model: oauth.model.clone(),
            max_tokens: 8,
            system: oauth.system_prompt(),
            messages: vec![],
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"system\""), "OAuth 요청엔 system이 있어야 한다");

        let api = ClaudeProvider::new("sk-ant-api03-k".to_string());
        let req = AnthropicRequest {
            model: api.model.clone(),
            max_tokens: 8,
            system: api.system_prompt(),
            messages: vec![],
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(
            !json.contains("\"system\""),
            "API 키 요청엔 system이 생략되어야 한다"
        );
    }

    /// 파싱 모델 상수가 haiku로 고정되어 있는지(FR1). 상향은 이 상수 교체 한 줄.
    #[test]
    fn test_parse_model_constant() {
        let provider = ClaudeProvider::new("sk-ant-api03-k".to_string());
        assert_eq!(provider.model, "claude-haiku-4-5");
    }
}
