use anyhow::{Context, Result};
use qdrant_client::Qdrant;
use qdrant_client::qdrant::{
    vectors_config::Config, CreateCollectionBuilder, Distance, PointStruct,
    SearchPointsBuilder, VectorParamsBuilder, VectorsConfig, Condition,
    PointId, Value, UpsertPointsBuilder, DeletePointsBuilder,
    ScrollPointsBuilder, CreateFieldIndexCollectionBuilder,
    FieldType, Filter,
};
use std::collections::{HashMap, HashSet};
use tokio::sync::RwLock;
use uuid::Uuid;

const VECTOR_SIZE: u64 = 3072; // Gemini embedding-001 dimension

/// 워크스페이스 ID로부터 Qdrant 컬렉션 이름을 생성한다.
/// 규칙: `documents_{workspace_id}`
pub fn collection_name(workspace_id: &str) -> String {
    format!("documents_{}", workspace_id)
}

/// 하나의 chunk(summary 또는 fact)에 대한 임베딩 데이터
pub struct ChunkData {
    pub chunk_type: String,
    pub chunk_index: usize,
    pub chunk_text: String,
    pub embedding: Vec<f32>,
}

pub struct QdrantStorage {
    client: Qdrant,
    /// 이미 존재를 확인한 컬렉션 캐시 (불필요한 Qdrant API 호출 방지)
    ensured_collections: RwLock<HashSet<String>>,
}

impl QdrantStorage {
    pub async fn new(url: &str) -> Result<Self> {
        // REST URL (6333) -> gRPC URL (6334)
        let grpc_url = url.replace(":6333", ":6334");

        let client = Qdrant::from_url(&grpc_url)
            .build()
            .context("Failed to create Qdrant client")?;

        let storage = Self {
            client,
            ensured_collections: RwLock::new(HashSet::new()),
        };

        // 하위 호환: default 워크스페이스 컬렉션 보장
        storage.ensure_collection("default").await?;

        Ok(storage)
    }

    /// 특정 워크스페이스의 컬렉션이 존재하는지 확인하고, 없으면 생성한다.
    /// 내부 캐시를 사용하여 중복 체크를 방지한다.
    pub async fn ensure_collection(&self, workspace_id: &str) -> Result<()> {
        let col_name = collection_name(workspace_id);

        // 캐시 히트 → 빠른 리턴
        {
            let cache = self.ensured_collections.read().await;
            if cache.contains(&col_name) {
                return Ok(());
            }
        }

        // Qdrant에서 실제 존재 여부 확인
        let collections = self.client.list_collections().await?;
        let exists = collections
            .collections
            .iter()
            .any(|c| c.name == col_name);

        if !exists {
            self.client
                .create_collection(
                    CreateCollectionBuilder::new(&col_name)
                        .vectors_config(VectorsConfig {
                            config: Some(Config::Params(
                                VectorParamsBuilder::new(VECTOR_SIZE, Distance::Cosine).build()
                            )),
                        })
                )
                .await
                .context("Failed to create collection")?;

            tracing::info!("Created Qdrant collection: {}", col_name);
        }

        // payload index 생성 (이미 존재하면 무시됨)
        let _ = self.client
            .create_field_index(
                CreateFieldIndexCollectionBuilder::new(
                    &col_name,
                    "document_id",
                    FieldType::Keyword,
                )
            )
            .await;

        let _ = self.client
            .create_field_index(
                CreateFieldIndexCollectionBuilder::new(
                    &col_name,
                    "chunk_type",
                    FieldType::Keyword,
                )
            )
            .await;

        // 캐시에 추가
        let mut cache = self.ensured_collections.write().await;
        cache.insert(col_name);

        Ok(())
    }

    /// 컬렉션을 삭제 후 재생성 (reindex 시 구 스키마 데이터 정리용)
    pub async fn recreate_collection(&self, workspace_id: &str) -> Result<()> {
        let col_name = collection_name(workspace_id);
        tracing::info!("Dropping and recreating Qdrant collection: {}", col_name);

        // 캐시에서 제거
        {
            let mut cache = self.ensured_collections.write().await;
            cache.remove(&col_name);
        }

        let _ = self.client.delete_collection(&col_name).await;
        self.ensure_collection(workspace_id).await?;

        tracing::info!("Collection recreated successfully: {}", col_name);
        Ok(())
    }

    /// 문서의 모든 chunk를 한 번에 upsert
    pub async fn upsert_chunks(
        &self,
        workspace_id: &str,
        document_id: Uuid,
        summary: &str,
        created_at: &str,
        chunks: Vec<ChunkData>,
    ) -> Result<()> {
        self.ensure_collection(workspace_id).await?;
        let col_name = collection_name(workspace_id);

        let points: Vec<PointStruct> = chunks
            .into_iter()
            .map(|chunk| {
                let mut payload: HashMap<String, Value> = HashMap::new();
                payload.insert("document_id".to_string(), Value::from(document_id.to_string()));
                payload.insert("chunk_type".to_string(), Value::from(chunk.chunk_type));
                payload.insert("chunk_index".to_string(), Value::from(chunk.chunk_index as i64));
                payload.insert("chunk_text".to_string(), Value::from(chunk.chunk_text));
                payload.insert("summary".to_string(), Value::from(summary.to_string()));
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
                .upsert_points(UpsertPointsBuilder::new(&col_name, points).wait(true))
                .await
                .context("Failed to upsert chunk points")?;
        }

        Ok(())
    }

    /// document_id 기준으로 해당 문서의 모든 chunk 삭제
    pub async fn delete_by_document_id(&self, workspace_id: &str, document_id: Uuid) -> Result<()> {
        let col_name = collection_name(workspace_id);

        let filter = qdrant_client::qdrant::Filter::must(vec![
            Condition::matches("document_id", document_id.to_string()),
        ]);

        self.client
            .delete_points(
                DeletePointsBuilder::new(&col_name)
                    .points(filter)
            )
            .await
            .context("Failed to delete points by document_id")?;

        Ok(())
    }

    pub async fn search(
        &self,
        workspace_id: &str,
        query_embedding: Vec<f32>,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let col_name = collection_name(workspace_id);

        let search_builder = SearchPointsBuilder::new(&col_name, query_embedding, limit as u64)
            .with_payload(true);

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

                Some(SearchHit {
                    id,
                    summary,
                    chunk_type,
                    chunk_text,
                    score: point.score,
                })
            })
            .collect();

        Ok(hits)
    }

    /// 워크스페이스 컬렉션 생성 (워크스페이스 생성 시 호출)
    pub async fn create_workspace_collection(&self, workspace_id: &str) -> Result<()> {
        self.ensure_collection(workspace_id).await
    }

    /// 워크스페이스 컬렉션 삭제 (워크스페이스 삭제 시 호출)
    pub async fn delete_workspace_collection(&self, workspace_id: &str) -> Result<()> {
        let col_name = collection_name(workspace_id);
        tracing::info!("Deleting Qdrant collection: {}", col_name);

        // 캐시에서 제거
        {
            let mut cache = self.ensured_collections.write().await;
            cache.remove(&col_name);
        }

        self.client
            .delete_collection(&col_name)
            .await
            .context("Failed to delete collection")?;

        tracing::info!("Deleted Qdrant collection: {}", col_name);
        Ok(())
    }

    /// 교차 워크스페이스 검색: 여러 컬렉션을 병렬 검색 후 RRF로 결합.
    pub async fn cross_workspace_search(
        &self,
        workspace_ids: &[String],
        query_embedding: Vec<f32>,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        use futures::future::join_all;

        let futures: Vec<_> = workspace_ids
            .iter()
            .map(|ws_id| {
                let embedding = query_embedding.clone();
                async move {
                    self.search(ws_id, embedding, limit).await
                }
            })
            .collect();

        let results: Vec<Result<Vec<SearchHit>>> = join_all(futures).await;

        let mut workspace_results = Vec::new();
        for result in results {
            match result {
                Ok(hits) => workspace_results.push(hits),
                Err(e) => {
                    tracing::warn!("Cross-workspace search partial failure: {}", e);
                }
            }
        }

        Ok(merge_cross_workspace_results(workspace_results, 60.0))
    }

    /// 모든 문서를 가져옴 (키워드 검색용)
    /// chunk_type_filter가 Some이면 해당 타입의 chunk만 반환
    pub async fn scroll_all(
        &self,
        workspace_id: &str,
        chunk_type_filter: Option<&str>,
    ) -> Result<Vec<SearchHit>> {
        let col_name = collection_name(workspace_id);
        let mut all_hits = Vec::new();
        let mut offset: Option<PointId> = None;

        loop {
            let mut scroll_builder = ScrollPointsBuilder::new(&col_name)
                .with_payload(true)
                .limit(100);

            if let Some(off) = offset.clone() {
                scroll_builder = scroll_builder.offset(off);
            }

            if let Some(ct) = chunk_type_filter {
                scroll_builder = scroll_builder.filter(Filter::must(vec![
                    Condition::matches("chunk_type", ct.to_string()),
                ]));
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

                all_hits.push(SearchHit {
                    id,
                    summary,
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


#[derive(Debug, Clone)]
pub struct SearchHit {
    pub id: Uuid,
    pub summary: String,
    pub chunk_type: String,
    pub chunk_text: String,
    pub score: f32,
}

/// 교차 워크스페이스 검색 결과를 RRF로 결합
fn merge_cross_workspace_results(workspace_results: Vec<Vec<SearchHit>>, k: f32) -> Vec<SearchHit> {
    use std::collections::HashMap;

    let mut rrf_scores: HashMap<String, (f32, SearchHit)> = HashMap::new();

    for hits in workspace_results {
        for (rank, hit) in hits.into_iter().enumerate() {
            let doc_key = hit.id.to_string();
            let rrf_score = 1.0 / (k + rank as f32 + 1.0);
            let entry = rrf_scores.entry(doc_key).or_insert((0.0, hit.clone()));
            entry.0 += rrf_score;
            // Keep the hit with the higher score
            if hit.score > entry.1.score {
                entry.1 = hit;
            }
        }
    }

    let mut results: Vec<(f32, SearchHit)> = rrf_scores.into_values().collect();
    results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    results.into_iter().map(|(_, hit)| hit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ──── collection_name 생성 규칙 ────

    #[test]
    fn test_collection_name_default() {
        assert_eq!(collection_name("default"), "documents_default");
    }

    #[test]
    fn test_collection_name_custom() {
        assert_eq!(collection_name("work"), "documents_work");
        assert_eq!(collection_name("my-workspace"), "documents_my-workspace");
        assert_eq!(collection_name("ws_123"), "documents_ws_123");
    }

    #[test]
    fn test_collection_name_deterministic() {
        let name1 = collection_name("test");
        let name2 = collection_name("test");
        assert_eq!(name1, name2);
    }

    #[test]
    fn test_collection_name_different_workspaces() {
        let name1 = collection_name("workspace-a");
        let name2 = collection_name("workspace-b");
        assert_ne!(name1, name2);
    }

    #[test]
    fn test_collection_name_prefix() {
        let name = collection_name("any");
        assert!(name.starts_with("documents_"));
    }
}
