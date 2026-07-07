use anyhow::{Context, Result};
use qdrant_client::Qdrant;
use qdrant_client::qdrant::{
    vectors_config::Config, CreateCollectionBuilder, Distance, PointStruct,
    SearchPointsBuilder, VectorParamsBuilder, VectorsConfig, Condition,
    PointId, Value, UpsertPointsBuilder, DeletePointsBuilder,
    ScrollPointsBuilder, CreateFieldIndexCollectionBuilder,
    FieldType, Filter, SetPayloadPointsBuilder,
};
use std::collections::{HashMap, HashSet};
use chrono::{DateTime, Utc};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::models::Edge;

/// 기본/레거시 임베딩 차원 (Gemini embedding-001 = 3072). 최초 기대 차원이며,
/// 임베딩 provider가 확정되면 `set_target_dim`으로 현재 provider 차원에 맞춘다(FR4).
const VECTOR_SIZE: u64 = 3072;

/// 워크스페이스 ID로부터 Qdrant 컬렉션 이름을 생성한다.
/// 규칙: `documents_{workspace_id}`
pub fn collection_name(workspace_id: &str) -> String {
    format!("documents_{}", workspace_id)
}

/// 기존 컬렉션 차원과 현재 provider 기대 차원을 대조한다(순수 함수 — 단위 테스트 대상).
///
/// - 컬렉션이 없으면(None) 통과 — 신규 생성 예정.
/// - 차원이 일치하면 통과.
/// - 불일치면 **침묵 실패·자동 재생성 없이** 명시적 에러(FR4). 재생성은 reindex의
///   명시적 책임이므로 여기서 데이터를 날리지 않는다.
fn dimension_check(existing: Option<u64>, target: u64) -> Result<()> {
    if let Some(dim) = existing {
        if dim != target {
            anyhow::bail!(
                "embedding dimension mismatch (collection={dim}d, provider={target}d) — run POST /api/reindex"
            );
        }
    }
    Ok(())
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
    /// 이미 존재+차원 일치를 확인한 컬렉션 캐시 (불필요한 Qdrant API 호출 방지).
    /// `set_target_dim`이 차원 변경 시 이 캐시를 비워, 캐시된 항목은 항상 현재
    /// 기대 차원과 일치가 확인된 것임을 보장한다.
    ensured_collections: RwLock<HashSet<String>>,
    /// 현재 임베딩 provider가 기대하는 벡터 차원. 컬렉션 생성·대조의 기준.
    target_dim: RwLock<u64>,
}

impl QdrantStorage {
    pub async fn new(url: &str) -> Result<Self> {
        // REST URL (6333) -> gRPC URL (6334)
        let grpc_url = url.replace(":6333", ":6334");

        let client = Qdrant::from_url(&grpc_url)
            .build()
            .context("Failed to create Qdrant client")?;

        // default 컬렉션 보장은 **lazy**로 미룬다(new에서 강제하지 않음). 시작 시점의
        // 기대 차원(VECTOR_SIZE)이 이미 마이그레이션된 컬렉션의 실제 차원과 다르면
        // 여기서 대조 에러가 나 서버가 못 뜨는 문제를 피한다. 컬렉션은 최초 검색/유입
        // (ensure_collection 경유) 시 현재 provider 차원으로 보장된다.
        Ok(Self {
            client,
            ensured_collections: RwLock::new(HashSet::new()),
            target_dim: RwLock::new(VECTOR_SIZE),
        })
    }

    /// 현재 임베딩 provider가 기대하는 벡터 차원을 설정한다(FR4).
    ///
    /// 차원이 바뀌면 "존재+차원 일치" 캐시를 무효화해, 다음 ensure가 실제 컬렉션과
    /// 다시 대조하도록 한다(전환 후 첫 검색이 불일치를 즉시 감지).
    pub async fn set_target_dim(&self, dim: u64) {
        let mut cur = self.target_dim.write().await;
        if *cur != dim {
            *cur = dim;
            self.ensured_collections.write().await.clear();
            tracing::info!("Qdrant 기대 임베딩 차원 변경: {}", dim);
        }
    }

    /// 컬렉션의 실제 벡터 차원을 조회한다(없으면 None). named-vector 구성은 None.
    async fn collection_dim(&self, col_name: &str) -> Result<Option<u64>> {
        let info = self
            .client
            .collection_info(col_name)
            .await
            .context("Failed to fetch collection info")?;
        let dim = info
            .result
            .and_then(|r| r.config)
            .and_then(|c| c.params)
            .and_then(|p| p.vectors_config)
            .and_then(|vc| vc.config)
            .and_then(|cfg| match cfg {
                Config::Params(vp) => Some(vp.size),
                Config::ParamsMap(_) => None,
            });
        Ok(dim)
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

        let target = *self.target_dim.read().await;

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
                                VectorParamsBuilder::new(target, Distance::Cosine).build()
                            )),
                        })
                )
                .await
                .context("Failed to create collection")?;

            tracing::info!("Created Qdrant collection: {} (dim={})", col_name, target);
        } else {
            // 존재하면 차원을 대조한다. 불일치면 명시적 "reindex 필요" 에러(FR4).
            // 이 캐시 미스 경로에서만 대조하고, 통과하면 아래에서 캐시에 넣어 이후
            // 호출은 빠르게 리턴한다(캐시는 set_target_dim이 차원 변경 시 비운다).
            let existing_dim = self.collection_dim(&col_name).await?;
            dimension_check(existing_dim, target)?;
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

    /// 컬렉션을 삭제 후 재생성한다 (reindex 시 구 스키마/구 차원 데이터 정리용).
    ///
    /// 재생성은 `ensure_collection`을 거치므로 **현재 `target_dim`**으로 만들어진다
    /// — 차원 마이그레이션(예: 3072→384)의 실행 지점(FR4). 호출 전에 embedder를
    /// 확보해 `set_target_dim`이 선행돼야 새 차원이 반영된다.
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

    /// 문서의 모든 chunk를 한 번에 upsert.
    ///
    /// `edges`는 문서의 그래프 간선으로, summary chunk payload에만 JSON 문자열로
    /// 비정규화된다(문서당 1곳). raw JSON이 SSoT이고 payload는 파생물이므로,
    /// reindex 시 raw의 `edges`가 다시 이 경로로 복원된다.
    pub async fn upsert_chunks(
        &self,
        workspace_id: &str,
        document_id: Uuid,
        summary: &str,
        created_at: &str,
        edges: &[Edge],
        chunks: Vec<ChunkData>,
    ) -> Result<()> {
        self.ensure_collection(workspace_id).await?;
        let col_name = collection_name(workspace_id);

        let edges_json = edges_to_payload(edges);

        let points: Vec<PointStruct> = chunks
            .into_iter()
            .map(|chunk| {
                let payload = build_chunk_payload(
                    document_id,
                    summary,
                    created_at,
                    &edges_json,
                    &chunk.chunk_type,
                    chunk.chunk_index,
                    &chunk.chunk_text,
                );
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

    /// 문서의 summary chunk payload에 있는 엣지만 재동기화한다(재임베딩 없음).
    ///
    /// 엣지 추가/제거 시 raw JSON을 먼저 갱신한 뒤 호출된다. `set_payload`로
    /// summary chunk의 `edges` 필드만 덮어써 벡터를 다시 계산하지 않는다(저비용).
    pub async fn update_edges_payload(
        &self,
        workspace_id: &str,
        document_id: Uuid,
        edges: &[Edge],
    ) -> Result<()> {
        self.ensure_collection(workspace_id).await?;
        let col_name = collection_name(workspace_id);

        let mut payload: HashMap<String, Value> = HashMap::new();
        payload.insert("edges".to_string(), Value::from(edges_to_payload(edges)));

        // 대상: 해당 문서의 summary chunk (document_id AND chunk_type=summary)
        let filter = Filter::must(vec![
            Condition::matches("document_id", document_id.to_string()),
            Condition::matches("chunk_type", "summary".to_string()),
        ]);

        self.client
            .set_payload(
                SetPayloadPointsBuilder::new(&col_name, payload)
                    .points_selector(filter)
                    .wait(true),
            )
            .await
            .context("Failed to update edges payload")?;

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
        // 컬렉션 존재·차원 대조를 먼저 수행한다. 차원 불일치면 여기서 "reindex 필요"
        // 에러가 나 침묵 실패를 막는다(FR4). 없으면 현재 차원으로 생성(빈 검색).
        self.ensure_collection(workspace_id).await?;
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
                let edges = extract_string(&payload, "edges")
                    .map(|s| edges_from_payload(&s))
                    .unwrap_or_default();
                let created_at = parse_created_at(&payload);

                Some(SearchHit {
                    id,
                    summary,
                    chunk_type,
                    chunk_text,
                    score: point.score,
                    edges,
                    created_at,
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
        // 키워드 검색 경로도 컬렉션 존재·차원 대조를 거친다(없으면 생성 → 빈 결과).
        self.ensure_collection(workspace_id).await?;
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
                let edges = extract_string(payload, "edges")
                    .map(|s| edges_from_payload(&s))
                    .unwrap_or_default();
                let created_at = parse_created_at(payload);

                all_hits.push(SearchHit {
                    id,
                    summary,
                    chunk_type,
                    chunk_text,
                    score: 0.0,
                    edges,
                    created_at,
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

/// 하나의 chunk에 대한 Qdrant point payload를 구성한다.
///
/// 엣지는 summary chunk에만 포함된다(문서당 1곳). 순수 함수로 분리해 "summary엔
/// edges, fact엔 없음" 불변식을 Qdrant 없이 단위 테스트로 고정한다 — 이것이
/// reindex 엣지 생존의 payload 측 절반이다(나머지 절반은 raw JSON의 edges 보존).
fn build_chunk_payload(
    document_id: Uuid,
    summary: &str,
    created_at: &str,
    edges_json: &str,
    chunk_type: &str,
    chunk_index: usize,
    chunk_text: &str,
) -> HashMap<String, Value> {
    let mut payload: HashMap<String, Value> = HashMap::new();
    payload.insert("document_id".to_string(), Value::from(document_id.to_string()));
    payload.insert("chunk_type".to_string(), Value::from(chunk_type.to_string()));
    payload.insert("chunk_index".to_string(), Value::from(chunk_index as i64));
    payload.insert("chunk_text".to_string(), Value::from(chunk_text.to_string()));
    payload.insert("summary".to_string(), Value::from(summary.to_string()));
    payload.insert("created_at".to_string(), Value::from(created_at.to_string()));
    if chunk_type == "summary" {
        payload.insert("edges".to_string(), Value::from(edges_json.to_string()));
    }
    payload
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
    /// summary chunk payload에서 파싱한 비정규화 엣지 (fact chunk에서는 빈 벡터).
    pub edges: Vec<Edge>,
    /// payload의 created_at (rfc3339) 파싱값. 시간 인식 검색용. 파싱 불가 시 None.
    pub created_at: Option<DateTime<Utc>>,
}

/// payload의 created_at 문자열(rfc3339)을 UTC DateTime으로 파싱한다.
fn parse_created_at(payload: &HashMap<String, Value>) -> Option<DateTime<Utc>> {
    extract_string(payload, "created_at")
        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

/// 엣지 목록을 payload 저장용 JSON 문자열로 직렬화한다.
///
/// 직렬화가 실패해도(사실상 불가능) 빈 배열로 폴백해 인덱싱을 막지 않는다 —
/// payload는 파생물이므로 최악의 경우에도 raw JSON에서 reindex로 복원된다.
pub fn edges_to_payload(edges: &[Edge]) -> String {
    serde_json::to_string(edges).unwrap_or_else(|_| "[]".to_string())
}

/// payload의 edges JSON 문자열을 엣지 목록으로 역직렬화한다.
///
/// 파싱 실패 시 빈 목록으로 폴백한다(raw JSON이 SSoT이므로 안전).
pub fn edges_from_payload(s: &str) -> Vec<Edge> {
    serde_json::from_str(s).unwrap_or_default()
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

    // ──── 차원 대조 (FR4 마이그레이션 불변식) ────

    #[test]
    fn test_dimension_check_missing_collection_passes() {
        // 컬렉션이 없으면(None) 통과 — 신규 생성 예정.
        assert!(dimension_check(None, 384).is_ok());
    }

    #[test]
    fn test_dimension_check_matching_passes() {
        assert!(dimension_check(Some(3072), 3072).is_ok());
        assert!(dimension_check(Some(384), 384).is_ok());
    }

    #[test]
    fn test_dimension_check_mismatch_errors_with_reindex_hint() {
        // gemini(3072) 인덱스 상태에서 local(384)로 전환 후 reindex 전 → 명시적 에러.
        let err = dimension_check(Some(3072), 384).unwrap_err().to_string();
        assert!(
            err.contains("dimension mismatch"),
            "차원 불일치를 명시해야 한다: {err}"
        );
        assert!(
            err.contains("reindex"),
            "reindex 필요를 안내해야 한다(침묵 실패 금지): {err}"
        );
    }

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

    // ──── 엣지 payload 직렬화 왕복 (비정규화 불변식) ────

    #[test]
    fn test_edges_payload_roundtrip() {
        use crate::models::{Edge, RelationType};
        let target = Uuid::new_v4();
        let edges = vec![
            Edge::new(target, RelationType::Updates, 0.8),
            Edge::new(Uuid::new_v4(), RelationType::RelatedTo, 0.5),
        ];
        let json = edges_to_payload(&edges);
        let restored = edges_from_payload(&json);

        assert_eq!(restored.len(), 2);
        assert_eq!(restored[0].target, target);
        assert_eq!(restored[0].relation, RelationType::Updates);
        assert!((restored[0].weight - 0.8).abs() < f32::EPSILON);
    }

    #[test]
    fn test_edges_payload_empty() {
        let json = edges_to_payload(&[]);
        assert_eq!(json, "[]");
        assert!(edges_from_payload(&json).is_empty());
    }

    #[test]
    fn test_edges_from_payload_malformed_is_empty() {
        // 손상된/빈 payload는 빈 목록으로 폴백한다 (raw JSON이 SSoT라 안전).
        assert!(edges_from_payload("not valid json").is_empty());
        assert!(edges_from_payload("").is_empty());
        assert!(edges_from_payload("{}").is_empty());
    }

    // ──── payload 빌드 (reindex 생존의 payload 측 불변식) ────

    #[test]
    fn test_build_chunk_payload_summary_includes_edges() {
        use crate::models::{Edge, RelationType};
        let doc_id = Uuid::new_v4();
        let target = Uuid::new_v4();
        let edges_json = edges_to_payload(&[Edge::new(target, RelationType::Updates, 0.5)]);

        let payload = build_chunk_payload(
            doc_id, "sum", "2026-01-01T00:00:00Z", &edges_json, "summary", 0, "text",
        );

        let stored = extract_string(&payload, "edges").expect("summary chunk엔 edges 있어야");
        let restored = edges_from_payload(&stored);
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].target, target);
        assert_eq!(restored[0].relation, RelationType::Updates);
    }

    #[test]
    fn test_build_chunk_payload_fact_omits_edges() {
        // fact chunk엔 edges 키가 없어야 한다 (문서당 1곳 원칙).
        let doc_id = Uuid::new_v4();
        let payload = build_chunk_payload(
            doc_id, "sum", "2026-01-01T00:00:00Z", "[]", "fact", 1, "a fact",
        );
        assert!(extract_string(&payload, "edges").is_none(), "fact chunk엔 edges가 없어야 한다");
        assert_eq!(extract_string(&payload, "chunk_type").as_deref(), Some("fact"));
        assert_eq!(
            extract_string(&payload, "document_id").as_deref(),
            Some(doc_id.to_string().as_str())
        );
    }

    #[test]
    fn test_build_chunk_payload_reindex_survival() {
        // reindex 생존의 payload 측: raw의 edges가 그대로 payload로 매핑되어야 한다.
        // (이진 정확 가중치로 f32 왕복 이슈를 배제한다.)
        use crate::models::{Edge, RelationType};
        let edges = vec![
            Edge::new(Uuid::new_v4(), RelationType::RelatedTo, 0.5),
            Edge::new(Uuid::new_v4(), RelationType::PartOf, 0.25),
        ];
        let edges_json = edges_to_payload(&edges);

        let payload = build_chunk_payload(Uuid::new_v4(), "s", "t", &edges_json, "summary", 0, "c");
        let restored = edges_from_payload(&extract_string(&payload, "edges").unwrap());

        assert_eq!(restored, edges, "raw edges가 payload 왕복 후에도 동일해야 한다(reindex 생존)");
    }
}
