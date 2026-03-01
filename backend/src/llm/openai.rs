use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::models::{Entity, ParsedContent};
use super::{LlmProvider, EmbeddingProvider, ProviderType, build_parse_prompt, extract_json, parse_entity_type};

const OPENAI_API_BASE: &str = "https://api.openai.com/v1";

/// OpenAI Chat Provider (GPT)
pub struct OpenAiChatProvider {
    client: Client,
    api_key: String,
    model: String,
}

impl OpenAiChatProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            model: "gpt-4o-mini".to_string(),
        }
    }
}

#[async_trait]
impl LlmProvider for OpenAiChatProvider {
    fn provider_type(&self) -> ProviderType {
        ProviderType::OpenAi
    }

    async fn parse(&self, content: &str) -> Result<ParsedContent> {
        let prompt = build_parse_prompt(content);

        let request = ChatRequest {
            model: self.model.clone(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: prompt,
            }],
            response_format: Some(ResponseFormat {
                r#type: "json_object".to_string(),
            }),
        };

        let response = self
            .client
            .post(format!("{}/chat/completions", OPENAI_API_BASE))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to call OpenAI API")?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            anyhow::bail!("OpenAI API error ({}): {}", status, error_text);
        }

        let response: ChatResponse = response
            .json()
            .await
            .context("Failed to parse OpenAI response")?;

        let text = response
            .choices
            .first()
            .map(|c| c.message.content.as_str())
            .unwrap_or("{}");

        let json_str = extract_json(text);

        let parsed: ParsedContentRaw = serde_json::from_str(json_str)
            .context("Failed to parse LLM response as JSON")?;

        Ok(ParsedContent {
            summary: parsed.summary,
            tags: parsed.tags,
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
        let response = self
            .client
            .get(format!("{}/models", OPENAI_API_BASE))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await?;

        Ok(response.status().is_success())
    }
}

/// OpenAI Embedding Provider
pub struct OpenAiEmbeddingProvider {
    client: Client,
    api_key: String,
    model: String,
}

impl OpenAiEmbeddingProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            model: "text-embedding-3-small".to_string(),
        }
    }
}

#[async_trait]
impl EmbeddingProvider for OpenAiEmbeddingProvider {
    fn provider_type(&self) -> ProviderType {
        ProviderType::OpenAi
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let request = EmbeddingRequest {
            model: self.model.clone(),
            input: text.to_string(),
        };

        let response = self
            .client
            .post(format!("{}/embeddings", OPENAI_API_BASE))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to call OpenAI Embeddings API")?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            anyhow::bail!("OpenAI Embedding API error ({}): {}", status, error_text);
        }

        let response: EmbeddingResponse = response
            .json()
            .await
            .context("Failed to parse OpenAI response")?;

        response
            .data
            .into_iter()
            .next()
            .map(|d| d.embedding)
            .context("No embedding returned")
    }

    fn dimension(&self) -> usize {
        1536 // text-embedding-3-small dimension
    }

    async fn validate_api_key(&self) -> Result<bool> {
        let response = self
            .client
            .get(format!("{}/models", OPENAI_API_BASE))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await?;

        Ok(response.status().is_success())
    }
}

// Chat Request/Response
#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
}

#[derive(Debug, Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct ResponseFormat {
    r#type: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    content: String,
}

// Embedding Request/Response
#[derive(Debug, Serialize)]
struct EmbeddingRequest {
    model: String,
    input: String,
}

#[derive(Debug, Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
}

#[derive(Debug, Deserialize)]
struct ParsedContentRaw {
    summary: String,
    tags: Vec<String>,
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
