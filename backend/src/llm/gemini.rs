use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::models::{Entity, ParsedContent};
use super::{LlmProvider, EmbeddingProvider, ProviderType, build_parse_prompt, extract_json, parse_entity_type};

const GEMINI_API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Gemini LLM Provider
pub struct GeminiProvider {
    client: Client,
    api_key: String,
    model: String,
}

impl GeminiProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            model: "gemini-2.5-flash".to_string(),
        }
    }
}

#[async_trait]
impl LlmProvider for GeminiProvider {
    fn provider_type(&self) -> ProviderType {
        ProviderType::Gemini
    }

    async fn parse(&self, content: &str) -> Result<ParsedContent> {
        let prompt = build_parse_prompt(content);

        let url = format!(
            "{}/models/{}:generateContent?key={}",
            GEMINI_API_BASE, self.model, self.api_key
        );

        let request = GeminiRequest {
            contents: vec![Content {
                parts: vec![Part { text: prompt }],
            }],
            generation_config: Some(GenerationConfig {
                temperature: Some(0.1),
                response_mime_type: Some("application/json".to_string()),
            }),
        };

        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .context("Failed to call Gemini API")?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            anyhow::bail!("Gemini API error ({}): {}", status, error_text);
        }

        let response: GeminiResponse = response
            .json()
            .await
            .context("Failed to parse Gemini response")?;

        let text = response
            .candidates
            .first()
            .and_then(|c| c.content.parts.first())
            .map(|p| p.text.as_str())
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
        let url = format!(
            "{}/models?key={}",
            GEMINI_API_BASE, self.api_key
        );

        let response = self.client.get(&url).send().await?;
        Ok(response.status().is_success())
    }
}

/// Gemini Embedding Provider
pub struct GeminiEmbeddingProvider {
    client: Client,
    api_key: String,
    model: String,
}

impl GeminiEmbeddingProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            model: "gemini-embedding-001".to_string(),
        }
    }
}

#[async_trait]
impl EmbeddingProvider for GeminiEmbeddingProvider {
    fn provider_type(&self) -> ProviderType {
        ProviderType::Gemini
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let url = format!(
            "{}/models/{}:embedContent?key={}",
            GEMINI_API_BASE, self.model, self.api_key
        );

        let request = GeminiEmbedRequest {
            model: format!("models/{}", self.model),
            content: Content {
                parts: vec![Part { text: text.to_string() }],
            },
        };

        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .context("Failed to call Gemini Embedding API")?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            anyhow::bail!("Gemini Embedding API error ({}): {}", status, error_text);
        }

        let response: GeminiEmbedResponse = response
            .json()
            .await
            .context("Failed to parse Gemini embedding response")?;

        Ok(response.embedding.values)
    }

    fn dimension(&self) -> usize {
        3072 // gemini-embedding-001 actual dimension
    }

    async fn validate_api_key(&self) -> Result<bool> {
        let url = format!(
            "{}/models?key={}",
            GEMINI_API_BASE, self.api_key
        );

        let response = self.client.get(&url).send().await?;
        Ok(response.status().is_success())
    }
}

// Request/Response 구조체
#[derive(Debug, Serialize)]
struct GeminiRequest {
    contents: Vec<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config: Option<GenerationConfig>,
}

#[derive(Debug, Serialize)]
struct GenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_mime_type: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Content {
    parts: Vec<Part>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Part {
    text: String,
}

#[derive(Debug, Deserialize)]
struct GeminiResponse {
    candidates: Vec<Candidate>,
}

#[derive(Debug, Deserialize)]
struct Candidate {
    content: Content,
}

#[derive(Debug, Serialize)]
struct GeminiEmbedRequest {
    model: String,
    content: Content,
}

#[derive(Debug, Deserialize)]
struct GeminiEmbedResponse {
    embedding: EmbeddingValues,
}

#[derive(Debug, Deserialize)]
struct EmbeddingValues {
    values: Vec<f32>,
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
