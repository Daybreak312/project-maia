//! Codex(ChatGPT 구독 OAuth) 파싱 전용 provider.
//!
//! 소유자의 ChatGPT 구독으로 파싱을 수행한다 — 종량제 키 없이. 토큰 획득은
//! 소유자 CLI(`codex login`)가 하고, Maia는 `~/.codex/auth.json`을 임포트해
//! **수명 관리(refresh)** 만 책임진다.
//!
//! 업스트림은 **비공식**(openai/codex CLI가 쓰는 백엔드)이므로 엔드포인트·헤더·
//! client_id를 [`upstream`] 단일 상수 모듈에 격리하고, 포맷 드리프트에 대비해
//! SSE 파서를 관대하게 두고 최종 폴백(raw 저장)을 상위에서 보장한다.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use base64::Engine as _;
use chrono::{DateTime, Utc};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::models::ParsedContent;
use super::{build_parse_prompt, parse_llm_json, LlmProvider, ProviderType};

/// Codex 비공식 업스트림 상수. **출처:** openai/codex CLI (codex-rs). **검증일:** 2026-07.
///
/// 비공식 계약이라 언제든 바뀔 수 있다 — 드리프트가 관측되면 이 한 곳만 고친다.
mod upstream {
    /// OAuth refresh 토큰 엔드포인트.
    pub const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
    /// Codex Responses API 엔드포인트(SSE).
    pub const RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
    /// Codex CLI OAuth client_id.
    pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
    /// refresh 요청 scope.
    pub const SCOPE: &str = "openid profile email";
    /// 파싱 모델(상향은 이 한 줄 교체).
    pub const MODEL: &str = "gpt-5.1";
    /// 요청 출처 식별자(Codex CLI 위장).
    pub const ORIGINATOR: &str = "codex_cli_rs";
    /// Responses API 베타 헤더 값.
    pub const OPENAI_BETA: &str = "responses=experimental";
}

/// access_token exp가 이 여유(초) 이내로 임박하면 선제 refresh 한다.
const REFRESH_SKEW_SECS: i64 = 60;

// ──────────────────────────── 시크릿 마스킹 ────────────────────────────

/// 시크릿 프리뷰: 앞 4 + 뒤 4 문자만 노출(그 사이는 `...`). 8자 이하는 전량 마스킹.
/// 로그·에러·API 응답 어디에도 원문 토큰이 나가지 않게 하는 단일 지점.
pub(crate) fn preview(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    if chars.len() <= 8 {
        return "****".to_string();
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}...{tail}")
}

// ──────────────────────────── 토큰 상태 ────────────────────────────

/// Codex OAuth 토큰 상태. 기존 설정 저장소(JSON, DATA_DIR)와 동일한 직렬화 규약.
#[derive(Clone, Serialize, Deserialize)]
pub struct CodexAuth {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
    pub account_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_refresh: Option<DateTime<Utc>>,
}

/// Debug 출력에서 토큰을 마스킹한다(시크릿 로깅 금지 불변식).
impl std::fmt::Debug for CodexAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexAuth")
            .field("access_token", &preview(&self.access_token))
            .field("refresh_token", &preview(&self.refresh_token))
            .field("id_token", &self.id_token.as_deref().map(preview))
            .field("account_id", &self.account_id)
            .field("last_refresh", &self.last_refresh)
            .finish()
    }
}

impl CodexAuth {
    /// 계정 프리뷰(account_id 마스킹). settings 응답용.
    pub fn account_preview(&self) -> String {
        preview(&self.account_id)
    }
}

// ──────────────────────────── auth.json 파싱 ────────────────────────────

#[derive(Default, Deserialize)]
struct AuthTokens {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
}

#[derive(Deserialize)]
struct AuthJsonFile {
    #[serde(default)]
    tokens: Option<AuthTokens>,
    // flat 폴백(일부 버전은 tokens 중첩 없이 최상위에 둔다).
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    last_refresh: Option<String>,
}

/// `~/.codex/auth.json` 원문을 관대하게 파싱한다.
///
/// tokens 중첩/flat 양쪽을 지원하고, account_id가 없으면 id_token JWT의
/// `chatgpt_account_id` 클레임에서 유도한다. access/refresh 토큰은 필수.
pub fn parse_codex_auth_json(raw: &str) -> Result<CodexAuth> {
    let file: AuthJsonFile =
        serde_json::from_str(raw).context("auth.json 파싱 실패 (유효한 JSON이 아닙니다)")?;
    let tokens = file.tokens.unwrap_or_default();

    let access_token = tokens
        .access_token
        .or(file.access_token)
        .filter(|s| !s.is_empty())
        .context("auth.json에 access_token이 없습니다")?;
    let refresh_token = tokens
        .refresh_token
        .or(file.refresh_token)
        .filter(|s| !s.is_empty())
        .context("auth.json에 refresh_token이 없습니다")?;
    let id_token = tokens.id_token.or(file.id_token).filter(|s| !s.is_empty());
    let account_id = tokens
        .account_id
        .or(file.account_id)
        .or_else(|| id_token.as_deref().and_then(account_id_from_id_token))
        .filter(|s| !s.is_empty())
        .context("auth.json에서 account_id를 찾을 수 없습니다 (tokens.account_id 또는 id_token 클레임 필요)")?;
    let last_refresh = file
        .last_refresh
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc));

    Ok(CodexAuth {
        access_token,
        refresh_token,
        id_token,
        account_id,
        last_refresh,
    })
}

/// JWT payload(가운데 세그먼트)를 base64url 디코드해 JSON으로 반환한다.
fn decode_jwt_payload(token: &str) -> Option<serde_json::Value> {
    let payload_b64 = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// access_token JWT의 `exp`(만료 unix 초)를 파싱한다. 없으면 None(만료 판단 불가).
pub fn jwt_exp(token: &str) -> Option<i64> {
    decode_jwt_payload(token)?.get("exp")?.as_i64()
}

/// id_token JWT에서 `chatgpt_account_id` 클레임을 유도한다(직접/네임스페이스 both).
fn account_id_from_id_token(token: &str) -> Option<String> {
    let v = decode_jwt_payload(token)?;
    if let Some(id) = v.get("chatgpt_account_id").and_then(|x| x.as_str()) {
        return Some(id.to_string());
    }
    // 네임스페이스 클레임: "https://api.openai.com/auth" → { chatgpt_account_id }
    v.get("https://api.openai.com/auth")
        .and_then(|a| a.get("chatgpt_account_id"))
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
}

/// 현재 시각 기준으로 access_token을 선제 refresh 해야 하는지 판정(순수 함수).
///
/// exp가 `skew`초 이내로 임박하면 true. exp를 파싱할 수 없으면 false — 일단
/// 써보고 401이 오면 그때 강제 refresh 하는 경로에 맡긴다(과잉 refresh 방지).
fn needs_refresh(auth: &CodexAuth, now: i64, skew: i64) -> bool {
    match jwt_exp(&auth.access_token) {
        Some(exp) => exp - now <= skew,
        None => false,
    }
}

// ──────────────────────────── 토큰 저장소 ────────────────────────────

/// Codex 토큰 영속 계층 추상화. 프로덕션은 `SettingsManager`가 구현하고,
/// 테스트는 인메모리 mock을 쓴다. `refresh_lock`은 모든 provider 인스턴스가
/// **공유**하는 단일 플라이트 게이트다(공유되지 않으면 직렬화가 깨진다).
#[async_trait]
pub trait CodexTokenStore: Send + Sync {
    async fn load(&self) -> Option<CodexAuth>;
    async fn save(&self, auth: CodexAuth) -> Result<()>;
    fn refresh_lock(&self) -> Arc<Mutex<()>>;
}

// ──────────────────────────── Provider ────────────────────────────

/// Codex 업스트림 엔드포인트(테스트에서 wiremock으로 주입).
#[derive(Clone)]
pub struct CodexEndpoints {
    pub token_url: String,
    pub responses_url: String,
}

impl Default for CodexEndpoints {
    fn default() -> Self {
        Self {
            token_url: upstream::TOKEN_URL.to_string(),
            responses_url: upstream::RESPONSES_URL.to_string(),
        }
    }
}

/// Codex 파싱 provider. `store`를 공유해 refresh 단일 플라이트를 보장한다.
#[derive(Clone)]
pub struct CodexProvider {
    store: Arc<dyn CodexTokenStore>,
    client: Client,
    endpoints: CodexEndpoints,
    model: String,
}

/// responses 호출의 실패 종류. 401은 강제 refresh 후 1회 재시도 대상이라 구분한다.
enum CodexCallError {
    Unauthorized,
    Other(anyhow::Error),
}

impl CodexCallError {
    fn into_anyhow(self) -> anyhow::Error {
        match self {
            CodexCallError::Unauthorized => anyhow!("codex 401 (인증 실패)"),
            CodexCallError::Other(e) => e,
        }
    }
}

impl CodexProvider {
    pub fn new(store: Arc<dyn CodexTokenStore>, client: Client) -> Self {
        Self {
            store,
            client,
            endpoints: CodexEndpoints::default(),
            model: upstream::MODEL.to_string(),
        }
    }

    /// 엔드포인트를 주입하는 생성자(테스트 전용 — wiremock URL 주입).
    pub fn with_endpoints(
        store: Arc<dyn CodexTokenStore>,
        client: Client,
        endpoints: CodexEndpoints,
    ) -> Self {
        Self {
            store,
            client,
            endpoints,
            model: upstream::MODEL.to_string(),
        }
    }

    /// 유효한 auth를 확보한다 — exp 임박이면 단일 플라이트 refresh.
    async fn ensure_valid(&self) -> Result<CodexAuth> {
        let auth = self
            .store
            .load()
            .await
            .ok_or_else(|| anyhow!("codex 미임포트 — auth.json 재임포트가 필요합니다"))?;
        if !needs_refresh(&auth, Utc::now().timestamp(), REFRESH_SKEW_SECS) {
            return Ok(auth);
        }
        self.refresh_under_lock(false).await
    }

    /// 단일 플라이트 refresh. 락 획득 후 재확인해(다른 태스크가 이미 갱신했을 수
    /// 있음) 중복 refresh를 막는다. `force=true`면 재확인 없이 무조건 refresh(401 대응).
    async fn refresh_under_lock(&self, force: bool) -> Result<CodexAuth> {
        let lock = self.store.refresh_lock();
        let _guard = lock.lock().await;

        let auth = self
            .store
            .load()
            .await
            .ok_or_else(|| anyhow!("codex 미임포트 — 재임포트가 필요합니다"))?;

        // 락을 기다리는 동안 다른 태스크가 이미 refresh 했다면 그 결과를 재사용.
        if !force && !needs_refresh(&auth, Utc::now().timestamp(), REFRESH_SKEW_SECS) {
            return Ok(auth);
        }

        let refreshed = self.do_refresh(&auth).await?;
        self.store.save(refreshed.clone()).await?;
        Ok(refreshed)
    }

    /// 실제 refresh HTTP. 회전된 refresh_token이 오면 [`apply_refresh`]가 영속화한다.
    async fn do_refresh(&self, auth: &CodexAuth) -> Result<CodexAuth> {
        let body = RefreshRequest {
            client_id: upstream::CLIENT_ID,
            grant_type: "refresh_token",
            refresh_token: &auth.refresh_token,
            scope: upstream::SCOPE,
        };

        let resp = self
            .client
            .post(&self.endpoints.token_url)
            .json(&body)
            .send()
            .await
            .context("codex refresh 요청 실패")?;

        let status = resp.status();
        if !status.is_success() {
            // 응답 본문은 시크릿 인접이라 로깅하지 않는다.
            anyhow::bail!("codex refresh 실패 ({status}) — auth.json 재임포트가 필요합니다");
        }

        let parsed: RefreshResponse = resp
            .json()
            .await
            .context("codex refresh 응답 파싱 실패")?;
        Ok(apply_refresh(auth, parsed, Utc::now()))
    }

    /// responses 호출 + 401 시 강제 refresh 후 1회 재시도.
    async fn call_responses(&self, instructions: &str, user_text: &str) -> Result<String> {
        let auth = self.ensure_valid().await?;
        match self.responses_once(&auth, instructions, user_text).await {
            Ok(text) => Ok(text),
            Err(CodexCallError::Unauthorized) => {
                let auth = self.refresh_under_lock(true).await?;
                self.responses_once(&auth, instructions, user_text)
                    .await
                    .map_err(CodexCallError::into_anyhow)
            }
            Err(other) => Err(other.into_anyhow()),
        }
    }

    /// responses 1회 호출. SSE 본문을 관대하게 집계한다. 401만 별도 분기.
    async fn responses_once(
        &self,
        auth: &CodexAuth,
        instructions: &str,
        user_text: &str,
    ) -> Result<String, CodexCallError> {
        let request = ResponsesRequest {
            model: &self.model,
            instructions,
            input: vec![InputItem {
                role: "user",
                content: vec![InputContent {
                    content_type: "input_text",
                    text: user_text,
                }],
            }],
            stream: true,
            store: false,
        };

        let resp = self
            .client
            .post(&self.endpoints.responses_url)
            .header("authorization", format!("Bearer {}", auth.access_token))
            .header("chatgpt-account-id", &auth.account_id)
            .header("OpenAI-Beta", upstream::OPENAI_BETA)
            .header("originator", upstream::ORIGINATOR)
            .header("session_id", Uuid::new_v4().to_string())
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .json(&request)
            .send()
            .await
            .map_err(|e| CodexCallError::Other(anyhow!("codex responses 요청 실패: {e}")))?;

        let status = resp.status();
        if status == StatusCode::UNAUTHORIZED {
            return Err(CodexCallError::Unauthorized);
        }
        if !status.is_success() {
            return Err(CodexCallError::Other(anyhow!(
                "codex responses 에러 ({status})"
            )));
        }

        // 전체 타임아웃(300초)은 build_http_client가 강제한다 — 무한 스트림 방지.
        let body = resp
            .text()
            .await
            .map_err(|e| CodexCallError::Other(anyhow!("codex 응답 본문 읽기 실패: {e}")))?;
        aggregate_sse(&body).map_err(CodexCallError::Other)
    }
}

#[async_trait]
impl LlmProvider for CodexProvider {
    fn provider_type(&self) -> ProviderType {
        ProviderType::Codex
    }

    async fn parse(&self, content: &str) -> Result<ParsedContent> {
        let prompt = build_parse_prompt(content);
        let text = self
            .call_responses("You are a precise structured-data extractor.", &prompt)
            .await?;
        parse_llm_json(&text)
    }

    async fn complete(&self, prompt: &str) -> Result<String> {
        self.call_responses("You are a helpful assistant.", prompt)
            .await
    }

    async fn validate_api_key(&self) -> Result<bool> {
        // refresh 포함 검증: 유효 토큰 확보 + 소형 responses 핑.
        let text = self
            .call_responses("You are a helper.", "Reply with the single word OK.")
            .await?;
        Ok(!text.trim().is_empty())
    }
}

/// refresh 응답을 기존 auth에 병합한다(순수 함수).
///
/// 회전된 refresh_token/id_token이 오면 반영하고, 없으면 기존값을 유지한다.
/// account_id는 refresh로 바뀌지 않으므로 그대로 둔다.
fn apply_refresh(old: &CodexAuth, resp: RefreshResponse, now: DateTime<Utc>) -> CodexAuth {
    CodexAuth {
        access_token: resp.access_token,
        refresh_token: resp
            .refresh_token
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| old.refresh_token.clone()),
        id_token: resp.id_token.or_else(|| old.id_token.clone()),
        account_id: old.account_id.clone(),
        last_refresh: Some(now),
    }
}

/// Codex Responses SSE 본문을 관대하게 집계한다(순수 함수 — 단위 테스트 핵심).
///
/// - `response.output_text.delta` → `delta` 누적
/// - `response.completed` → 종결(누적본 우선, 비면 output에서 추출)
/// - `response.failed` → 에러
/// - 그 외/알 수 없는 이벤트 → 무시(포맷 드리프트 관대 처리)
/// - `[DONE]` 센티넬 → 무시
pub fn aggregate_sse(body: &str) -> Result<String> {
    let mut out = String::new();

    for line in body.lines() {
        let line = line.trim();
        let Some(data) = line.strip_prefix("data:") else {
            continue; // event: 라인 등은 무시하고 data: 페이로드만 본다
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
            continue; // 파싱 불가 페이로드는 관대하게 건너뛴다
        };

        match value.get("type").and_then(|t| t.as_str()) {
            Some("response.output_text.delta") => {
                if let Some(delta) = value.get("delta").and_then(|d| d.as_str()) {
                    out.push_str(delta);
                }
            }
            Some("response.completed") => {
                if !out.is_empty() {
                    return Ok(out);
                }
                if let Some(text) = extract_completed_text(&value) {
                    return Ok(text);
                }
                anyhow::bail!("codex 완료 이벤트에 출력 텍스트가 없습니다");
            }
            Some("response.failed") => {
                let msg = value
                    .pointer("/response/error/message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("codex 응답 실패");
                anyhow::bail!("codex responses 실패: {msg}");
            }
            _ => { /* 알 수 없는 이벤트 무시 */ }
        }
    }

    if out.is_empty() {
        anyhow::bail!("codex SSE에서 출력 텍스트를 얻지 못했습니다");
    }
    Ok(out)
}

/// `response.completed` 이벤트의 `response.output[*].content[*].text`에서 텍스트를
/// 긁어모은다(델타 누적이 비어 있을 때의 폴백).
fn extract_completed_text(value: &serde_json::Value) -> Option<String> {
    let output = value.pointer("/response/output")?.as_array()?;
    let mut text = String::new();
    for item in output {
        if let Some(content) = item.get("content").and_then(|c| c.as_array()) {
            for block in content {
                if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                    text.push_str(t);
                }
            }
        }
    }
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

// ──────────────────────────── 요청/응답 스키마 ────────────────────────────

#[derive(Serialize)]
struct RefreshRequest<'a> {
    client_id: &'a str,
    grant_type: &'a str,
    refresh_token: &'a str,
    scope: &'a str,
}

#[derive(Deserialize)]
struct RefreshResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
}

#[derive(Serialize)]
struct ResponsesRequest<'a> {
    model: &'a str,
    instructions: &'a str,
    input: Vec<InputItem<'a>>,
    stream: bool,
    store: bool,
}

#[derive(Serialize)]
struct InputItem<'a> {
    role: &'a str,
    content: Vec<InputContent<'a>>,
}

#[derive(Serialize)]
struct InputContent<'a> {
    #[serde(rename = "type")]
    content_type: &'a str,
    text: &'a str,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// exp를 지정한 최소 JWT를 만든다(header.payload.sig). jwt_exp는 payload만 본다.
    fn make_jwt_with_exp(exp: i64) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"{\"alg\":\"none\"}");
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(format!("{{\"exp\":{exp}}}").as_bytes());
        format!("{header}.{payload}.sig")
    }

    fn make_id_token_with_account(account: &str) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"{\"alg\":\"none\"}");
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(format!("{{\"chatgpt_account_id\":\"{account}\"}}").as_bytes());
        format!("{header}.{payload}.sig")
    }

    // ──── 시크릿 마스킹 ────

    #[test]
    fn test_preview_masks_secret() {
        assert_eq!(preview("short"), "****");
        assert_eq!(preview("12345678"), "****");
        assert_eq!(preview("abcdefghijklmnop"), "abcd...mnop");
    }

    #[test]
    fn test_codex_auth_debug_does_not_leak_token() {
        let auth = CodexAuth {
            access_token: "sk-super-secret-access-token-value".to_string(),
            refresh_token: "rt-super-secret-refresh-token-value".to_string(),
            id_token: Some("id-super-secret-token-value".to_string()),
            account_id: "acct-1234567890".to_string(),
            last_refresh: None,
        };
        let dbg = format!("{auth:?}");
        assert!(!dbg.contains("super-secret"), "Debug에 원문 토큰이 노출되면 안 된다: {dbg}");
        assert!(dbg.contains("sk-s...alue")); // 마스킹된 프리뷰
    }

    // ──── auth.json 파싱 ────

    #[test]
    fn test_parse_auth_json_nested_tokens() {
        let raw = r#"{
            "tokens": {
                "access_token": "at-abc",
                "refresh_token": "rt-abc",
                "id_token": "id-abc",
                "account_id": "acct-1"
            },
            "last_refresh": "2026-07-01T00:00:00Z"
        }"#;
        let auth = parse_codex_auth_json(raw).unwrap();
        assert_eq!(auth.access_token, "at-abc");
        assert_eq!(auth.refresh_token, "rt-abc");
        assert_eq!(auth.account_id, "acct-1");
        assert!(auth.last_refresh.is_some());
    }

    #[test]
    fn test_parse_auth_json_flat() {
        let raw = r#"{"access_token":"at-x","refresh_token":"rt-x","account_id":"acct-x"}"#;
        let auth = parse_codex_auth_json(raw).unwrap();
        assert_eq!(auth.access_token, "at-x");
        assert_eq!(auth.account_id, "acct-x");
    }

    #[test]
    fn test_parse_auth_json_account_from_id_token() {
        // account_id가 명시되지 않으면 id_token 클레임에서 유도한다.
        let id_token = make_id_token_with_account("acct-derived");
        let raw = format!(
            r#"{{"tokens":{{"access_token":"at","refresh_token":"rt","id_token":"{id_token}"}}}}"#
        );
        let auth = parse_codex_auth_json(&raw).unwrap();
        assert_eq!(auth.account_id, "acct-derived");
    }

    #[test]
    fn test_parse_auth_json_missing_access_token_errors() {
        let raw = r#"{"tokens":{"refresh_token":"rt","account_id":"a"}}"#;
        assert!(parse_codex_auth_json(raw).is_err());
    }

    #[test]
    fn test_parse_auth_json_invalid_json_errors() {
        assert!(parse_codex_auth_json("not json").is_err());
    }

    // ──── JWT exp ────

    #[test]
    fn test_jwt_exp_parses() {
        let token = make_jwt_with_exp(1_900_000_000);
        assert_eq!(jwt_exp(&token), Some(1_900_000_000));
    }

    #[test]
    fn test_jwt_exp_missing_returns_none() {
        // exp 없는 payload → None
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"{}");
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"{\"sub\":\"x\"}");
        let token = format!("{header}.{payload}.sig");
        assert_eq!(jwt_exp(&token), None);
        // 형식이 깨진 토큰도 안전하게 None
        assert_eq!(jwt_exp("garbage"), None);
    }

    // ──── needs_refresh (순수 상태 판정) ────

    #[test]
    fn test_needs_refresh_logic() {
        let auth = |exp| CodexAuth {
            access_token: make_jwt_with_exp(exp),
            refresh_token: "rt".to_string(),
            id_token: None,
            account_id: "a".to_string(),
            last_refresh: None,
        };
        let now = 1_000_000;
        // 만료 여유 충분 → refresh 불필요
        assert!(!needs_refresh(&auth(now + 3600), now, REFRESH_SKEW_SECS));
        // 60초 이내 임박 → refresh 필요
        assert!(needs_refresh(&auth(now + 30), now, REFRESH_SKEW_SECS));
        // 이미 만료 → refresh 필요
        assert!(needs_refresh(&auth(now - 10), now, REFRESH_SKEW_SECS));
    }

    #[test]
    fn test_needs_refresh_no_exp_is_false() {
        // exp를 못 읽으면 선제 refresh하지 않는다(401 경로에 위임).
        let auth = CodexAuth {
            access_token: "no-jwt".to_string(),
            refresh_token: "rt".to_string(),
            id_token: None,
            account_id: "a".to_string(),
            last_refresh: None,
        };
        assert!(!needs_refresh(&auth, 1_000_000, REFRESH_SKEW_SECS));
    }

    // ──── apply_refresh (회전 영속화) ────

    #[test]
    fn test_apply_refresh_rotates_refresh_token() {
        let old = CodexAuth {
            access_token: "old-at".to_string(),
            refresh_token: "old-rt".to_string(),
            id_token: Some("old-id".to_string()),
            account_id: "acct".to_string(),
            last_refresh: None,
        };
        let resp = RefreshResponse {
            access_token: "new-at".to_string(),
            refresh_token: Some("new-rt".to_string()),
            id_token: None,
        };
        let now = Utc::now();
        let merged = apply_refresh(&old, resp, now);
        assert_eq!(merged.access_token, "new-at");
        assert_eq!(merged.refresh_token, "new-rt", "회전된 refresh_token이 영속화돼야 한다");
        assert_eq!(merged.id_token.as_deref(), Some("old-id"), "id_token 미제공 시 기존 유지");
        assert_eq!(merged.account_id, "acct");
        assert_eq!(merged.last_refresh, Some(now));
    }

    #[test]
    fn test_apply_refresh_keeps_refresh_token_when_absent() {
        let old = CodexAuth {
            access_token: "old-at".to_string(),
            refresh_token: "old-rt".to_string(),
            id_token: None,
            account_id: "acct".to_string(),
            last_refresh: None,
        };
        let resp = RefreshResponse {
            access_token: "new-at".to_string(),
            refresh_token: None,
            id_token: None,
        };
        let merged = apply_refresh(&old, resp, Utc::now());
        assert_eq!(merged.refresh_token, "old-rt", "회전 미제공 시 기존 refresh_token 유지");
    }

    // ──── SSE 집계 ────

    #[test]
    fn test_aggregate_sse_deltas_and_completed() {
        let body = "\
event: response.output_text.delta
data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello\"}

data: {\"type\":\"response.output_text.delta\",\"delta\":\" World\"}

data: {\"type\":\"unknown.event\",\"foo\":1}

data: {\"type\":\"response.completed\",\"response\":{\"id\":\"x\"}}

data: [DONE]
";
        assert_eq!(aggregate_sse(body).unwrap(), "Hello World");
    }

    #[test]
    fn test_aggregate_sse_failed_errors() {
        let body = "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"boom\"}}}\n";
        let err = aggregate_sse(body).unwrap_err().to_string();
        assert!(err.contains("boom"), "실패 사유가 에러에 담겨야 한다: {err}");
    }

    #[test]
    fn test_aggregate_sse_completed_without_delta_uses_output() {
        // 델타 없이 completed에 통짜 output이 온 경우 폴백 추출.
        let body = "data: {\"type\":\"response.completed\",\"response\":{\"output\":[{\"content\":[{\"type\":\"output_text\",\"text\":\"full text\"}]}]}}\n";
        assert_eq!(aggregate_sse(body).unwrap(), "full text");
    }

    #[test]
    fn test_aggregate_sse_empty_errors() {
        // 아무 델타도 없고 종결도 없으면 명확한 에러(침묵 실패 금지).
        assert!(aggregate_sse("event: ping\n\n").is_err());
    }
}

/// Codex refresh 상태머신 + responses 호출을 wiremock으로 검증한다(라이브 토큰 금지).
#[cfg(test)]
mod http_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Mutex as AsyncMutex;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// exp가 far-future인 유효 access_token JWT.
    fn valid_access_jwt() -> String {
        let header =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"{\"alg\":\"none\"}");
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(b"{\"exp\":4000000000}");
        format!("{header}.{payload}.sig")
    }

    /// exp가 과거인 만료된 access_token JWT.
    fn expired_access_jwt() -> String {
        let header =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"{\"alg\":\"none\"}");
        let payload =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"{\"exp\":1000000}");
        format!("{header}.{payload}.sig")
    }

    /// SSE 본문(델타 + 완료)을 만든다. `text`가 최종 집계 결과가 된다.
    fn sse_body(text: &str) -> String {
        let delta = serde_json::json!({
            "type": "response.output_text.delta",
            "delta": text
        })
        .to_string();
        let completed = serde_json::json!({
            "type": "response.completed",
            "response": {"id": "resp_x"}
        })
        .to_string();
        format!("data: {delta}\n\ndata: {completed}\n\ndata: [DONE]\n")
    }

    /// 인메모리 토큰 저장소(단일 플라이트 락 공유 검증용).
    struct MockStore {
        auth: AsyncMutex<Option<CodexAuth>>,
        lock: Arc<Mutex<()>>,
        save_count: AtomicUsize,
    }

    impl MockStore {
        fn new(auth: CodexAuth) -> Arc<Self> {
            Arc::new(Self {
                auth: AsyncMutex::new(Some(auth)),
                lock: Arc::new(Mutex::new(())),
                save_count: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait]
    impl CodexTokenStore for MockStore {
        async fn load(&self) -> Option<CodexAuth> {
            self.auth.lock().await.clone()
        }
        async fn save(&self, auth: CodexAuth) -> Result<()> {
            self.save_count.fetch_add(1, Ordering::SeqCst);
            *self.auth.lock().await = Some(auth);
            Ok(())
        }
        fn refresh_lock(&self) -> Arc<Mutex<()>> {
            self.lock.clone()
        }
    }

    fn auth_with(access: String) -> CodexAuth {
        CodexAuth {
            access_token: access,
            refresh_token: "rt-original".to_string(),
            id_token: None,
            account_id: "acct-1".to_string(),
            last_refresh: None,
        }
    }

    fn provider(store: Arc<MockStore>, server: &MockServer) -> CodexProvider {
        CodexProvider::with_endpoints(
            store,
            reqwest::Client::new(),
            CodexEndpoints {
                token_url: format!("{}/oauth/token", server.uri()),
                responses_url: format!("{}/responses", server.uri()),
            },
        )
    }

    async fn count_requests(server: &MockServer, url_path: &str) -> usize {
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.url.path() == url_path)
            .count()
    }

    /// 유효 토큰 → refresh 없이 responses 호출.
    #[tokio::test]
    async fn test_valid_token_no_refresh() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_string(sse_body("hello")))
            .mount(&server)
            .await;

        let store = MockStore::new(auth_with(valid_access_jwt()));
        let provider = provider(store.clone(), &server);

        let out = provider.complete("hi").await.unwrap();
        assert_eq!(out, "hello");
        assert_eq!(count_requests(&server, "/oauth/token").await, 0, "유효 토큰은 refresh하지 않는다");
        assert_eq!(count_requests(&server, "/responses").await, 1);
        assert_eq!(store.save_count.load(Ordering::SeqCst), 0, "refresh 없으니 저장도 없다");
    }

    /// 만료 토큰 → refresh 후 responses 호출, 회전 refresh_token 영속화.
    #[tokio::test]
    async fn test_expired_token_refreshes_then_calls() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": valid_access_jwt(),
                "refresh_token": "rt-rotated"
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_string(sse_body("world")))
            .mount(&server)
            .await;

        let store = MockStore::new(auth_with(expired_access_jwt()));
        let provider = provider(store.clone(), &server);

        let out = provider.complete("hi").await.unwrap();
        assert_eq!(out, "world");
        assert_eq!(count_requests(&server, "/oauth/token").await, 1, "만료 시 refresh 1회");
        // 회전된 refresh_token이 저장됐는지 확인.
        let saved = store.load().await.unwrap();
        assert_eq!(saved.refresh_token, "rt-rotated", "회전된 refresh_token 영속화");
        assert!(saved.last_refresh.is_some());
    }

    /// refresh 실패 → 명확한 재임포트 에러(침묵 실패 금지).
    #[tokio::test]
    async fn test_refresh_failure_clear_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(400))
            .mount(&server)
            .await;

        let store = MockStore::new(auth_with(expired_access_jwt()));
        let provider = provider(store.clone(), &server);

        let err = provider.complete("hi").await.unwrap_err().to_string();
        assert!(err.contains("재임포트"), "refresh 실패는 재임포트 필요를 명시해야 한다: {err}");
    }

    /// 401 응답 → 강제 refresh 후 1회 재시도로 복구.
    #[tokio::test]
    async fn test_401_triggers_refresh_and_retry() {
        let server = MockServer::start().await;
        // 첫 responses 호출은 401(최대 1회), 이후 200.
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(401))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_string(sse_body("recovered")))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": valid_access_jwt()
            })))
            .mount(&server)
            .await;

        // 유효 토큰으로 시작 → 첫 호출 401 → force refresh → 재시도 성공.
        let store = MockStore::new(auth_with(valid_access_jwt()));
        let provider = provider(store, &server);

        let out = provider.complete("hi").await.unwrap();
        assert_eq!(out, "recovered");
        assert_eq!(count_requests(&server, "/oauth/token").await, 1, "401 시 강제 refresh 1회");
        assert_eq!(count_requests(&server, "/responses").await, 2, "401 후 재시도로 총 2회");
    }

    /// 단일 플라이트: 동시 호출들이 refresh를 중복 실행하지 않는다.
    #[tokio::test]
    async fn test_refresh_single_flight() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "access_token": valid_access_jwt() }))
                    // refresh 경합 창을 넓혀 단일 플라이트가 실제로 작동하는지 본다.
                    .set_delay(std::time::Duration::from_millis(80)),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_string(sse_body("ok")))
            .mount(&server)
            .await;

        let store = MockStore::new(auth_with(expired_access_jwt()));
        let provider = Arc::new(provider(store.clone(), &server));

        // 8개 동시 파싱 호출 — 만료 토큰이라 모두 refresh가 필요해 보인다.
        let mut handles = Vec::new();
        for _ in 0..8 {
            let p = provider.clone();
            handles.push(tokio::spawn(async move { p.complete("hi").await }));
        }
        for h in handles {
            h.await.unwrap().unwrap();
        }

        assert_eq!(
            count_requests(&server, "/oauth/token").await,
            1,
            "동시 8개 호출이 refresh를 단 1회만 실행해야 한다(단일 플라이트)"
        );
        assert_eq!(store.save_count.load(Ordering::SeqCst), 1, "저장도 1회");
    }

    /// 파싱 경로: SSE로 온 JSON을 ParsedContent로 변환한다.
    #[tokio::test]
    async fn test_parse_path_from_sse() {
        let server = MockServer::start().await;
        let inner = r#"{"summary":"요약","entities":[],"facts":["사실1","사실2"]}"#;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_string(sse_body(inner)))
            .mount(&server)
            .await;

        let store = MockStore::new(auth_with(valid_access_jwt()));
        let provider = provider(store, &server);

        let parsed = provider.parse("원문 텍스트").await.unwrap();
        assert_eq!(parsed.summary, "요약");
        assert_eq!(parsed.facts, vec!["사실1".to_string(), "사실2".to_string()]);
    }
}
