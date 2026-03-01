use anyhow::{Context, Result};
use qdrant_client::Qdrant;
use qdrant_client::qdrant::{
    vectors_config::Config, CreateCollectionBuilder, Distance, PointStruct,
    SearchPointsBuilder, VectorParamsBuilder, VectorsConfig, Condition,
    PointId, Value, UpsertPointsBuilder, DeletePointsBuilder,
    ScrollPointsBuilder, CreateFieldIndexCollectionBuilder,
    FieldType, Filter, condition::ConditionOneOf,
};
use std::collections::HashMap;
use uuid::Uuid;

const COLLECTION_NAME: &str = "documents";
const VECTOR_SIZE: u64 = 3072; // Gemini embedding-001 dimension

/// 하나의 chunk(summary 또는 fact)에 대한 임베딩 데이터
pub struct ChunkData {
    pub chunk_type: String,
    pub chunk_index: usize,
    pub chunk_text: String,
    pub embedding: Vec<f32>,
}

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

        // payload index 생성 (이미 존재하면 무시됨)
        let _ = self.client
            .create_field_index(
                CreateFieldIndexCollectionBuilder::new(
                    COLLECTION_NAME,
                    "document_id",
                    FieldType::Keyword,
                )
            )
            .await;

        let _ = self.client
            .create_field_index(
                CreateFieldIndexCollectionBuilder::new(
                    COLLECTION_NAME,
                    "chunk_type",
                    FieldType::Keyword,
                )
            )
            .await;

        Ok(())
    }

    /// 컬렉션을 삭제 후 재생성 (reindex 시 구 스키마 데이터 정리용)
    pub async fn recreate_collection(&self) -> Result<()> {
        tracing::info!("Dropping and recreating Qdrant collection: {}", COLLECTION_NAME);

        let _ = self.client.delete_collection(COLLECTION_NAME).await;
        self.ensure_collection().await?;

        tracing::info!("Collection recreated successfully");
        Ok(())
    }

    /// 문서의 모든 chunk를 한 번에 upsert
    pub async fn upsert_chunks(
        &self,
        document_id: Uuid,
        summary: &str,
        tags: &[String],
        created_at: &str,
        chunks: Vec<ChunkData>,
    ) -> Result<()> {
        let points: Vec<PointStruct> = chunks
            .into_iter()
            .map(|chunk| {
                let mut payload: HashMap<String, Value> = HashMap::new();
                payload.insert("document_id".to_string(), Value::from(document_id.to_string()));
                payload.insert("chunk_type".to_string(), Value::from(chunk.chunk_type));
                payload.insert("chunk_index".to_string(), Value::from(chunk.chunk_index as i64));
                payload.insert("chunk_text".to_string(), Value::from(chunk.chunk_text));
                payload.insert("summary".to_string(), Value::from(summary.to_string()));
                payload.insert("tags".to_string(), Value::from(tags.to_vec()));
                payload.insert("created_at".to_string(), Value::from(created_at.to_string()));

                let point_id = Uuid::new_v4();
                PointStruct::new(
                    PointId::from(point_id.to_string()),
                    chunk.embedding,
                    payload,
                )
            })
            .collect();

        if !points.is_empty() {
            self.client
                .upsert_points(UpsertPointsBuilder::new(COLLECTION_NAME, points).wait(true))
                .await
                .context("Failed to upsert chunk points")?;
        }

        Ok(())
    }

    /// document_id 기준으로 해당 문서의 모든 chunk 삭제
    pub async fn delete_by_document_id(&self, document_id: Uuid) -> Result<()> {
        let filter = qdrant_client::qdrant::Filter::must(vec![
            Condition::matches("document_id", document_id.to_string()),
        ]);

        self.client
            .delete_points(
                DeletePointsBuilder::new(COLLECTION_NAME)
                    .points(filter)
            )
            .await
            .context("Failed to delete points by document_id")?;

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
                search_builder = search_builder.filter(Filter::should(conditions));
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
                let payload = point.payload;

                let document_id = extract_string(&payload, "document_id")?;
                let id = Uuid::parse_str(&document_id).ok()?;
                let chunk_type = extract_string(&payload, "chunk_type").unwrap_or_default();
                let chunk_text = extract_string(&payload, "chunk_text").unwrap_or_default();
                let summary = extract_string(&payload, "summary").unwrap_or_default();
                let tags = extract_string_list(&payload, "tags");

                Some(SearchHit {
                    id,
                    summary,
                    tags,
                    chunk_type,
                    chunk_text,
                    score: point.score,
                })
            })
            .collect();

        Ok(hits)
    }

    /// 모든 문서를 가져옴 (키워드 검색용)
    /// chunk_type_filter가 Some이면 해당 타입의 chunk만 반환
    pub async fn scroll_all(
        &self,
        tags_filter: Option<Vec<String>>,
        chunk_type_filter: Option<&str>,
    ) -> Result<Vec<SearchHit>> {
        let mut all_hits = Vec::new();
        let mut offset: Option<PointId> = None;

        loop {
            let mut scroll_builder = ScrollPointsBuilder::new(COLLECTION_NAME)
                .with_payload(true)
                .limit(100);

            if let Some(off) = offset.clone() {
                scroll_builder = scroll_builder.offset(off);
            }

            let mut must_conditions: Vec<Condition> = Vec::new();

            if let Some(ref tags) = tags_filter {
                let tag_conditions: Vec<Condition> = tags
                    .iter()
                    .map(|tag| Condition::matches("tags", tag.clone()))
                    .collect();

                if !tag_conditions.is_empty() {
                    // tags OR 조건을 nested filter로 must에 추가
                    must_conditions.push(Condition {
                        condition_one_of: Some(ConditionOneOf::Filter(
                            Filter::should(tag_conditions)
                        )),
                    });
                }
            }

            if let Some(ct) = chunk_type_filter {
                must_conditions.push(Condition::matches("chunk_type", ct.to_string()));
            }

            if !must_conditions.is_empty() {
                scroll_builder = scroll_builder.filter(Filter::must(must_conditions));
            }

            let response = self.client
                .scroll(scroll_builder)
                .await
                .context("Failed to scroll points")?;

            if response.result.is_empty() {
                break;
            }

            for point in &response.result {
                let payload = &point.payload;

                let document_id = match extract_string(payload, "document_id") {
                    Some(s) => s,
                    None => continue,
                };
                let id = match Uuid::parse_str(&document_id) {
                    Ok(id) => id,
                    Err(_) => continue,
                };

                let chunk_type = extract_string(payload, "chunk_type").unwrap_or_default();
                let chunk_text = extract_string(payload, "chunk_text").unwrap_or_default();
                let summary = extract_string(payload, "summary").unwrap_or_default();
                let tags = extract_string_list(payload, "tags");

                all_hits.push(SearchHit {
                    id,
                    summary,
                    tags,
                    chunk_type,
                    chunk_text,
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

/// payload에서 문자열 추출 헬퍼
fn extract_string(payload: &HashMap<String, Value>, key: &str) -> Option<String> {
    payload.get(key)
        .and_then(|v| v.kind.as_ref())
        .and_then(|k| match k {
            qdrant_client::qdrant::value::Kind::StringValue(s) => Some(s.clone()),
            _ => None,
        })
}

/// payload에서 문자열 리스트 추출 헬퍼
fn extract_string_list(payload: &HashMap<String, Value>, key: &str) -> Vec<String> {
    payload.get(key)
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
        .unwrap_or_default()
}

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub id: Uuid,
    pub summary: String,
    pub tags: Vec<String>,
    pub chunk_type: String,
    pub chunk_text: String,
    pub score: f32,
}
