use anyhow::{Context, Result};
use qdrant_client::Qdrant;
use qdrant_client::qdrant::{
    vectors_config::Config, CreateCollectionBuilder, Distance, PointStruct,
    SearchPointsBuilder, VectorParamsBuilder, VectorsConfig, Condition,
    PointId, Value, UpsertPointsBuilder, DeletePointsBuilder, PointsIdsList,
    ScrollPointsBuilder,
};
use std::collections::HashMap;
use uuid::Uuid;

use crate::models::Document;

const COLLECTION_NAME: &str = "documents";
const VECTOR_SIZE: u64 = 3072; // Gemini embedding-001 dimension

pub struct QdrantStorage {
    client: Qdrant,
}

impl QdrantStorage {
    pub async fn new(url: &str) -> Result<Self> {
        // REST URL (6333) -> gRPC URL (6334)
        let grpc_url = url.replace(":6333", ":6334");

        let client = Qdrant::from_url(&grpc_url)
            .build()
            .context("Failed to create Qdrant client")?;

        let storage = Self { client };
        storage.ensure_collection().await?;

        Ok(storage)
    }

    async fn ensure_collection(&self) -> Result<()> {
        let collections = self.client.list_collections().await?;

        let exists = collections
            .collections
            .iter()
            .any(|c| c.name == COLLECTION_NAME);

        if !exists {
            self.client
                .create_collection(
                    CreateCollectionBuilder::new(COLLECTION_NAME)
                        .vectors_config(VectorsConfig {
                            config: Some(Config::Params(
                                VectorParamsBuilder::new(VECTOR_SIZE, Distance::Cosine).build()
                            )),
                        })
                )
                .await
                .context("Failed to create collection")?;

            tracing::info!("Created Qdrant collection: {}", COLLECTION_NAME);
        }

        Ok(())
    }

    pub async fn upsert(&self, doc: &Document, embedding: Vec<f32>) -> Result<()> {
        let mut payload: HashMap<String, Value> = HashMap::new();
        payload.insert("raw_content".to_string(), Value::from(doc.raw_content.clone()));
        payload.insert("summary".to_string(), Value::from(doc.summary.clone()));
        payload.insert("tags".to_string(), Value::from(doc.tags.clone()));
        payload.insert("created_at".to_string(), Value::from(doc.created_at.to_rfc3339()));

        let point = PointStruct::new(
            PointId::from(doc.id.to_string()),
            embedding,
            payload,
        );

        self.client
            .upsert_points(UpsertPointsBuilder::new(COLLECTION_NAME, vec![point]))
            .await
            .context("Failed to upsert point")?;

        Ok(())
    }

    pub async fn delete(&self, id: Uuid) -> Result<()> {
        let point_id = PointId::from(id.to_string());

        self.client
            .delete_points(
                DeletePointsBuilder::new(COLLECTION_NAME)
                    .points(PointsIdsList {
                        ids: vec![point_id],
                    })
            )
            .await
            .context("Failed to delete point from Qdrant")?;

        Ok(())
    }

    pub async fn search(
        &self,
        query_embedding: Vec<f32>,
        tags_filter: Option<Vec<String>>,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let mut search_builder = SearchPointsBuilder::new(COLLECTION_NAME, query_embedding, limit as u64)
            .with_payload(true);

        if let Some(tags) = tags_filter {
            let conditions: Vec<Condition> = tags
                .into_iter()
                .map(|tag| Condition::matches("tags", tag))
                .collect();

            if !conditions.is_empty() {
                search_builder = search_builder.filter(qdrant_client::qdrant::Filter::should(conditions));
            }
        }

        let response = self.client
            .search_points(search_builder)
            .await
            .context("Failed to search points")?;

        let hits = response
            .result
            .into_iter()
            .filter_map(|point| {
                let id_str = match &point.id {
                    Some(pid) => match &pid.point_id_options {
                        Some(qdrant_client::qdrant::point_id::PointIdOptions::Uuid(s)) => s.clone(),
                        _ => return None,
                    },
                    None => return None,
                };

                let id = Uuid::parse_str(&id_str).ok()?;
                let payload = point.payload;

                let raw_content = payload.get("raw_content")
                    .and_then(|v| v.kind.as_ref())
                    .and_then(|k| match k {
                        qdrant_client::qdrant::value::Kind::StringValue(s) => Some(s.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();

                let summary = payload.get("summary")
                    .and_then(|v| v.kind.as_ref())
                    .and_then(|k| match k {
                        qdrant_client::qdrant::value::Kind::StringValue(s) => Some(s.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();

                let tags: Vec<String> = payload.get("tags")
                    .and_then(|v| v.kind.as_ref())
                    .and_then(|k| match k {
                        qdrant_client::qdrant::value::Kind::ListValue(list) => {
                            Some(list.values.iter().filter_map(|v| {
                                match &v.kind {
                                    Some(qdrant_client::qdrant::value::Kind::StringValue(s)) => Some(s.clone()),
                                    _ => None,
                                }
                            }).collect())
                        },
                        _ => None,
                    })
                    .unwrap_or_default();

                Some(SearchHit {
                    id,
                    raw_content,
                    summary,
                    tags,
                    score: point.score,
                })
            })
            .collect();

        Ok(hits)
    }

    /// 모든 문서를 가져옴 (키워드 검색용)
    pub async fn scroll_all(&self, tags_filter: Option<Vec<String>>) -> Result<Vec<SearchHit>> {
        let mut all_hits = Vec::new();
        let mut offset: Option<PointId> = None;

        loop {
            let mut scroll_builder = ScrollPointsBuilder::new(COLLECTION_NAME)
                .with_payload(true)
                .limit(100);

            if let Some(off) = offset.clone() {
                scroll_builder = scroll_builder.offset(off);
            }

            if let Some(ref tags) = tags_filter {
                let conditions: Vec<Condition> = tags
                    .iter()
                    .map(|tag| Condition::matches("tags", tag.clone()))
                    .collect();

                if !conditions.is_empty() {
                    scroll_builder = scroll_builder.filter(qdrant_client::qdrant::Filter::should(conditions));
                }
            }

            let response = self.client
                .scroll(scroll_builder)
                .await
                .context("Failed to scroll points")?;

            if response.result.is_empty() {
                break;
            }

            for point in &response.result {
                let id_str = match &point.id {
                    Some(pid) => match &pid.point_id_options {
                        Some(qdrant_client::qdrant::point_id::PointIdOptions::Uuid(s)) => s.clone(),
                        _ => continue,
                    },
                    None => continue,
                };

                let id = match Uuid::parse_str(&id_str) {
                    Ok(id) => id,
                    Err(_) => continue,
                };
                let payload = &point.payload;

                let raw_content = payload.get("raw_content")
                    .and_then(|v| v.kind.as_ref())
                    .and_then(|k| match k {
                        qdrant_client::qdrant::value::Kind::StringValue(s) => Some(s.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();

                let summary = payload.get("summary")
                    .and_then(|v| v.kind.as_ref())
                    .and_then(|k| match k {
                        qdrant_client::qdrant::value::Kind::StringValue(s) => Some(s.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();

                let tags: Vec<String> = payload.get("tags")
                    .and_then(|v| v.kind.as_ref())
                    .and_then(|k| match k {
                        qdrant_client::qdrant::value::Kind::ListValue(list) => {
                            Some(list.values.iter().filter_map(|v| {
                                match &v.kind {
                                    Some(qdrant_client::qdrant::value::Kind::StringValue(s)) => Some(s.clone()),
                                    _ => None,
                                }
                            }).collect())
                        },
                        _ => None,
                    })
                    .unwrap_or_default();

                all_hits.push(SearchHit {
                    id,
                    raw_content,
                    summary,
                    tags,
                    score: 0.0,
                });
            }

            offset = response.next_page_offset;
            if offset.is_none() {
                break;
            }
        }

        Ok(all_hits)
    }
}

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub id: Uuid,
    pub raw_content: String,
    pub summary: String,
    pub tags: Vec<String>,
    pub score: f32,
}
