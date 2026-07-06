use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::models::{Entity, ParsedContent};
use super::{LlmProvider, ProviderType, build_parse_prompt, extract_json, parse_entity_type};

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";

/// Claude LLM Provider
pub struct ClaudeProvider {
    client: Client,
    api_key: String,
    model: String,
}

impl ClaudeProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            model: "claude-sonnet-4-20250514".to_string(),
        }
    }
}

#[async_trait]
impl LlmProvider for ClaudeProvider {
    fn provider_type(&self) -> ProviderType {
        ProviderType::Claude
    }

    async fn parse(&self, content: &str) -> Result<ParsedContent> {
        let prompt = build_parse_prompt(content);

        let request = AnthropicRequest {
            model: self.model.clone(),
            max_tokens: 1024,
            messages: vec![Message {
                role: "user".to_string(),
                content: prompt,
            }],
        };

        let response = self
            .client
            .post(ANTHROPIC_API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
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

        let text = response
            .content
            .first()
            .map(|c| c.text.as_str())
            .unwrap_or("{}");

        let json_str = extract_json(text);

        let parsed: ParsedContentRaw = serde_json::from_str(json_str)
            .context("Failed to parse LLM response as JSON")?;

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

    async fn validate_api_key(&self) -> Result<bool> {
        // 간단한 검증: 빈 메시지로 API 호출 시도
        let request = AnthropicRequest {
            model: self.model.clone(),
            max_tokens: 1,
            messages: vec![Message {
                role: "user".to_string(),
                content: "Hi".to_string(),
            }],
        };

        let response = self
            .client
            .post(ANTHROPIC_API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
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

#[derive(Debug, Deserialize)]
struct ParsedContentRaw {
    summary: String,
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
