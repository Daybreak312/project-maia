use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use crate::core::search::{BM25Scorer, SearchMode, reciprocal_rank_fusion};
use crate::llm::{create_llm_provider, create_embedding_provider, LlmProvider, EmbeddingProvider};
use crate::models::{Document, api::{IngestResponse, SearchResponse, SearchResult}};
use crate::settings::SettingsManager;
use crate::storage::{DocumentStore, QdrantStorage, SearchHit, ChunkData};

/// RRF 상수 (일반적으로 60 사용)
const RRF_K: f32 = 60.0;

/// Qdrant 검색 시 사전 필터 (이 이하는 후보에서 제외)
const MIN_VECTOR_SCORE: f32 = 0.3;

/// 최종 결과에 포함할 최소 관련도 (raw cosine similarity 기준)
const MIN_RELEVANCE_SCORE: f32 = 0.5;

/// 연속 결과 간 점수 차이가 이 값을 넘으면 하위 결과 제거
const SCORE_DROP_THRESHOLD: f32 = 0.15;

/// 단일 검색의 최대 결과 수
const MAX_RESULTS: usize = 5;

/// 워크스페이스 미지정 시 사용되는 기본 워크스페이스 ID
const DEFAULT_WORKSPACE: &str = "default";

/// 모든 핵심 로직을 오케스트레이션하는 인덱서
pub struct Indexer {
    settings: Arc<SettingsManager>,
    qdrant: Arc<QdrantStorage>,
    documents: Arc<DocumentStore>,
}

impl Indexer {
    pub fn new(
        settings: Arc<SettingsManager>,
        qdrant: Arc<QdrantStorage>,
        documents: Arc<DocumentStore>,
    ) -> Self {
        Self {
            settings,
            qdrant,
            documents,
        }
    }

    /// 현재 설정에 맞는 LLM Provider 생성
    async fn get_llm_provider(&self) -> Result<Box<dyn LlmProvider>> {
        let settings = self.settings.get().await;
        let provider_type = settings.parsing_provider;

        let api_key = settings
            .api_keys
            .get(&provider_type)
            .cloned()
            .ok_or_else(|| anyhow!("API key for {} is not configured", provider_type))?;

        Ok(create_llm_provider(provider_type, api_key))
    }

    /// 현재 설정에 맞는 Embedding Provider 생성
    async fn get_embedding_provider(&self) -> Result<Box<dyn EmbeddingProvider>> {
        let settings = self.settings.get().await;
        let provider_type = settings.embedding_provider;

        let api_key = settings
            .api_keys
            .get(&provider_type)
            .cloned()
            .ok_or_else(|| anyhow!("API key for {} is not configured", provider_type))?;

        Ok(create_embedding_provider(provider_type, api_key))
    }

    /// ParsedContent + summary 텍스트로부터 ChunkData 벡터를 생성
    async fn build_chunks(
        &self,
        embedder: &dyn EmbeddingProvider,
        summary: &str,
        facts: &[String],
    ) -> Result<Vec<ChunkData>> {
        let mut chunks = Vec::new();

        // summary chunk (항상 생성)
        let summary_embedding = embedder.embed(summary).await?;
        chunks.push(ChunkData {
            chunk_type: "summary".to_string(),
            chunk_index: 0,
            chunk_text: summary.to_string(),
            embedding: summary_embedding,
        });

        // fact chunks
        for (i, fact) in facts.iter().enumerate() {
            match embedder.embed(fact).await {
                Ok(embedding) => {
                    chunks.push(ChunkData {
                        chunk_type: "fact".to_string(),
                        chunk_index: i + 1,
                        chunk_text: fact.clone(),
                        embedding,
                    });
                }
                Err(e) => {
                    tracing::warn!("Failed to embed fact[{}], skipping: {}", i, e);
                }
            }
        }

        Ok(chunks)
    }

    /// 자연어 입력 → 파싱 → 임베딩 → 저장
    pub async fn ingest(&self, raw_content: String) -> Result<IngestResponse> {
        self.ingest_to_workspace(raw_content, DEFAULT_WORKSPACE).await
    }

    /// 특정 워크스페이스에 문서를 인제스트한다.
    pub async fn ingest_to_workspace(&self, raw_content: String, workspace_id: &str) -> Result<IngestResponse> {
        // 1. LLM으로 파싱
        tracing::info!("Parsing content...");
        let llm = self.get_llm_provider().await?;
        let parsed = llm.parse(&raw_content).await?;

        // 2. 문서 생성
        let doc = Document::new(
            raw_content,
            parsed.summary.clone(),
            parsed.entities.clone(),
            parsed.facts.clone(),
        );

        // 3. 임베딩 생성 (summary + facts)
        tracing::info!("Generating embeddings for {} chunks...", 1 + parsed.facts.len());
        let embedder = self.get_embedding_provider().await?;
        let chunks = self.build_chunks(embedder.as_ref(), &parsed.summary, &parsed.facts).await?;

        // 4. 파일 시스템에 원본 저장
        tracing::info!("Saving document...");
        self.documents.save(&doc, workspace_id).await?;

        // 5. Qdrant에 chunk 벡터 저장
        tracing::info!("Indexing {} chunks to Qdrant (workspace: {})...", chunks.len(), workspace_id);
        self.qdrant.upsert_chunks(
            workspace_id,
            doc.id,
            &doc.summary,
            &doc.created_at.to_rfc3339(),
            chunks,
        ).await?;

        tracing::info!("Ingested document: {} ({} facts)", doc.id, doc.facts.len());

        Ok(IngestResponse {
            id: doc.id,
            summary: doc.summary,
            entities: doc.entities,
            facts: doc.facts,
        })
    }

    /// Hybrid Search: 벡터 검색 + 키워드 검색 결합
    pub async fn search(
        &self,
        query: String,
        limit: usize,
        offset: usize,
        mode: Option<String>,
    ) -> Result<SearchResponse> {
        self.search_in_workspace(query, limit, offset, mode, DEFAULT_WORKSPACE).await
    }

    /// 특정 워크스페이스에서 검색한다.
    pub async fn search_in_workspace(
        &self,
        query: String,
        limit: usize,
        offset: usize,
        mode: Option<String>,
        workspace_id: &str,
    ) -> Result<SearchResponse> {
        let search_mode: SearchMode = mode
            .as_deref()
            .and_then(|m| m.parse().ok())
            .unwrap_or_default();

        tracing::info!("Search mode: {:?}, query: {}, workspace: {}", search_mode, query, workspace_id);

        let (results, _total) = match search_mode {
            SearchMode::Vector => self.vector_search(&query, limit + offset, workspace_id).await?,
            SearchMode::Keyword => self.keyword_search(&query, limit + offset, workspace_id).await?,
            SearchMode::Hybrid => self.hybrid_search(&query, limit + offset, workspace_id).await?,
        };

        // 관련성 기반 동적 필터링
        let filtered = filter_by_relevance(results);
        let filtered_total = filtered.len();

        // 페이지네이션 적용
        let paginated: Vec<_> = filtered
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect();

        let sources_used: Vec<_> = paginated.iter().map(|r| r.id).collect();

        Ok(SearchResponse {
            results: paginated,
            sources_used,
            total: filtered_total,
            mode: format!("{:?}", search_mode).to_lowercase(),
        })
    }

    /// 벡터 검색 — chunk 단위 검색 후 document_id 기준 그룹핑
    async fn vector_search(
        &self,
        query: &str,
        limit: usize,
        workspace_id: &str,
    ) -> Result<(Vec<SearchResult>, usize)> {
        let embedder = self.get_embedding_provider().await?;
        let query_embedding = embedder.embed(query).await?;

        // over-fetch: 같은 문서의 여러 chunk가 히트할 수 있으므로
        let hits = self.qdrant.search(workspace_id, query_embedding, limit * 5).await?;

        // document_id 기준 그룹핑
        let mut groups: HashMap<Uuid, DocumentGroup> = HashMap::new();

        for hit in hits {
            if hit.score < MIN_VECTOR_SCORE {
                continue;
            }

            let group = groups.entry(hit.id).or_insert_with(|| DocumentGroup {
                summary: hit.summary.clone(),
                best_score: 0.0,
                matched_facts: Vec::new(),
            });

            if hit.score > group.best_score {
                group.best_score = hit.score;
            }

            if hit.chunk_type == "fact" {
                group.matched_facts.push(hit.chunk_text);
            }
        }

        // 최고 점수 기준 정렬
        let mut sorted: Vec<_> = groups.into_iter().collect();
        sorted.sort_by(|a, b| b.1.best_score.partial_cmp(&a.1.best_score).unwrap_or(std::cmp::Ordering::Equal));

        let total = sorted.len();

        let results: Vec<SearchResult> = sorted
            .into_iter()
            .take(limit)
            .map(|(id, group)| SearchResult {
                id,
                summary: group.summary,
                relevance_score: group.best_score,
                matched_facts: group.matched_facts,
            })
            .collect();

        Ok((results, total))
    }

    /// 키워드 검색 (BM25) — summary chunk만 대상
    async fn keyword_search(
        &self,
        query: &str,
        limit: usize,
        workspace_id: &str,
    ) -> Result<(Vec<SearchResult>, usize)> {
        // summary chunk만 가져오기
        let all_docs = self.qdrant.scroll_all(workspace_id, Some("summary")).await?;

        if all_docs.is_empty() {
            return Ok((vec![], 0));
        }

        // BM25 인덱스 구축
        let mut scorer = BM25Scorer::new();
        let mut doc_map: HashMap<String, SearchHit> = HashMap::new();

        for doc in all_docs {
            let id_str = doc.id.to_string();
            scorer.add_document(&id_str, &doc.chunk_text);
            doc_map.insert(id_str, doc);
        }

        // BM25 스코어 계산
        let scores = scorer.score(query);
        let total = scores.len();

        // 정규화 및 결과 변환
        let max_score = scores.first().map(|(_, s)| *s).unwrap_or(1.0).max(1.0);

        let results: Vec<SearchResult> = scores
            .into_iter()
            .take(limit)
            .filter_map(|(id, score)| {
                let doc = doc_map.get(&id)?;
                Some(SearchResult {
                    id: doc.id,
                    summary: doc.summary.clone(),
                    relevance_score: (score / max_score).min(1.0),
                    matched_facts: vec![],
                })
            })
            .collect();

        Ok((results, total))
    }

    /// 하이브리드 검색 (RRF로 결합)
    async fn hybrid_search(
        &self,
        query: &str,
        limit: usize,
        workspace_id: &str,
    ) -> Result<(Vec<SearchResult>, usize)> {
        // 병렬로 두 검색 수행
        let (vector_results, keyword_results) = tokio::join!(
            self.vector_search(query, limit * 2, workspace_id),
            self.keyword_search(query, limit * 2, workspace_id)
        );

        let vector_results = vector_results.unwrap_or((vec![], 0)).0;
        let keyword_results = keyword_results.unwrap_or((vec![], 0)).0;

        // 벡터 검색의 raw cosine similarity 보존 (정직한 점수 표시용)
        let mut vector_scores: HashMap<String, f32> = HashMap::new();
        for r in &vector_results {
            vector_scores.insert(r.id.to_string(), r.relevance_score);
        }

        // 문서 정보 맵 구축 (vector 결과의 matched_facts 보존)
        let mut doc_map: HashMap<String, SearchResult> = HashMap::new();
        for r in vector_results.iter().chain(keyword_results.iter()) {
            doc_map.entry(r.id.to_string()).or_insert_with(|| r.clone());
        }

        // RRF로 순서 결정 (점수가 아닌 순서만 사용)
        let vector_ranking: Vec<_> = vector_results
            .iter()
            .map(|r| (r.id.to_string(), r.relevance_score))
            .collect();

        let keyword_ranking: Vec<_> = keyword_results
            .iter()
            .map(|r| (r.id.to_string(), r.relevance_score))
            .collect();

        let fused = reciprocal_rank_fusion(vec![vector_ranking, keyword_ranking], RRF_K);
        let total = fused.len();

        // RRF 순서를 유지하되, 점수는 raw cosine similarity 사용
        let results: Vec<SearchResult> = fused
            .into_iter()
            .take(limit)
            .filter_map(|(id, _rrf_score)| {
                let doc = doc_map.get(&id)?;
                // 벡터 검색에 있었으면 raw cosine, 키워드 전용이면 BM25 정규화 점수의 절반
                let score = vector_scores.get(&id).copied()
                    .unwrap_or(doc.relevance_score * 0.5);
                Some(SearchResult {
                    id: doc.id,
                    summary: doc.summary.clone(),
                    relevance_score: score,
                    matched_facts: doc.matched_facts.clone(),
                })
            })
            .collect();

        Ok((results, total))
    }

    /// 문서 조회
    pub async fn get_document(&self, id: uuid::Uuid) -> Result<Document> {
        self.documents.load(id, DEFAULT_WORKSPACE).await
    }

    /// 특정 워크스페이스에서 문서 조회
    pub async fn get_document_from_workspace(&self, id: uuid::Uuid, workspace_id: &str) -> Result<Document> {
        self.documents.load(id, workspace_id).await
    }

    /// 최근 문서 목록 (페이지네이션 지원)
    pub async fn recent(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<Document>, usize)> {
        let docs = self.documents.list_recent(1000, DEFAULT_WORKSPACE).await?;
        let total = docs.len();
        let paginated = docs.into_iter().skip(offset).take(limit).collect();

        Ok((paginated, total))
    }

    /// 문서 수정 (raw_content 변경 → 재파싱 + 재임베딩)
    pub async fn update(&self, id: uuid::Uuid, raw_content: String) -> Result<IngestResponse> {
        self.update_in_workspace(id, raw_content, DEFAULT_WORKSPACE).await
    }

    /// 특정 워크스페이스에서 문서를 수정한다.
    pub async fn update_in_workspace(&self, id: uuid::Uuid, raw_content: String, workspace_id: &str) -> Result<IngestResponse> {
        // 1. LLM으로 파싱
        tracing::info!("Re-parsing content for update...");
        let llm = self.get_llm_provider().await?;
        let parsed = llm.parse(&raw_content).await?;

        // 2. 기존 문서의 created_at 보존, updated_at 갱신
        let existing = self.documents.load(id, workspace_id).await?;
        let now = chrono::Utc::now();
        let doc = Document {
            id,
            raw_content,
            summary: parsed.summary.clone(),
            entities: parsed.entities.clone(),
            facts: parsed.facts.clone(),
            created_at: existing.created_at,
            updated_at: now,
        };

        // 3. 임베딩 생성 (summary + facts)
        tracing::info!("Re-generating embeddings...");
        let embedder = self.get_embedding_provider().await?;
        let chunks = self.build_chunks(embedder.as_ref(), &parsed.summary, &parsed.facts).await?;

        // 4. 파일 덮어쓰기
        tracing::info!("Saving updated document...");
        self.documents.save(&doc, workspace_id).await?;

        // 5. 기존 chunk 전부 삭제 후 새 chunk 저장
        tracing::info!("Updating Qdrant index...");
        self.qdrant.delete_by_document_id(workspace_id, id).await?;
        self.qdrant.upsert_chunks(
            workspace_id,
            id,
            &doc.summary,
            &doc.created_at.to_rfc3339(),
            chunks,
        ).await?;

        tracing::info!("Updated document: {} ({} facts)", doc.id, doc.facts.len());

        Ok(IngestResponse {
            id: doc.id,
            summary: doc.summary,
            entities: doc.entities,
            facts: doc.facts,
        })
    }

    /// 문서 삭제
    pub async fn delete(&self, id: uuid::Uuid) -> Result<()> {
        self.delete_from_workspace(id, DEFAULT_WORKSPACE).await
    }

    /// 특정 워크스페이스에서 문서를 삭제한다.
    pub async fn delete_from_workspace(&self, id: uuid::Uuid, workspace_id: &str) -> Result<()> {
        // 1. Qdrant에서 해당 문서의 모든 chunk 삭제
        tracing::info!("Deleting chunks from Qdrant...");
        self.qdrant.delete_by_document_id(workspace_id, id).await?;

        // 2. 파일 삭제
        tracing::info!("Deleting document file...");
        self.documents.delete(id, workspace_id).await?;

        tracing::info!("Deleted document: {}", id);
        Ok(())
    }

    /// 파일 시스템의 모든 문서를 Qdrant에 재인덱싱
    /// 컬렉션을 완전히 재생성하여 구 스키마 데이터도 정리
    pub async fn reindex_all(&self) -> Result<usize> {
        self.reindex_workspace(DEFAULT_WORKSPACE).await
    }

    /// 특정 워크스페이스의 Qdrant 컬렉션을 재인덱싱한다.
    pub async fn reindex_workspace(&self, workspace_id: &str) -> Result<usize> {
        let docs = self.documents.list_recent(10000, workspace_id).await?;
        let total = docs.len();

        if total == 0 {
            return Ok(0);
        }

        // 컬렉션 재생성 (구 스키마 orphan 포인트 정리)
        self.qdrant.recreate_collection(workspace_id).await?;

        let embedder = self.get_embedding_provider().await?;
        let mut indexed = 0;

        for doc in docs {
            // summary + facts 임베딩 (facts가 비어있으면 summary chunk만)
            match self.build_chunks(embedder.as_ref(), &doc.summary, &doc.facts).await {
                Ok(chunks) => {
                    let chunk_count = chunks.len();
                    if let Err(e) = self.qdrant.upsert_chunks(
                        workspace_id,
                        doc.id,
                        &doc.summary,
                        &doc.created_at.to_rfc3339(),
                        chunks,
                    ).await {
                        tracing::error!("Failed to index {}: {}", doc.id, e);
                    } else {
                        indexed += 1;
                        tracing::info!("Reindexed {}/{}: {} ({} chunks)", indexed, total, doc.id, chunk_count);
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to embed {}: {}", doc.id, e);
                }
            }
        }

        tracing::info!("Reindex complete: {}/{} documents", indexed, total);
        Ok(indexed)
    }

}

/// 검색 결과를 관련성 기준으로 동적 필터링
///
/// 세 가지 조건 중 가장 먼저 걸리는 것에서 잘라냄:
/// 1. 절대 임계값: MIN_RELEVANCE_SCORE 미만은 제거
/// 2. 점수 드롭: 연속 결과 간 차이가 SCORE_DROP_THRESHOLD를 넘으면 거기서 컷
/// 3. 상한: MAX_RESULTS 초과 제거
fn filter_by_relevance(results: Vec<SearchResult>) -> Vec<SearchResult> {
    if results.is_empty() {
        return results;
    }

    let mut filtered: Vec<SearchResult> = Vec::new();

    for (i, result) in results.into_iter().enumerate() {
        // 상한 도달
        if i >= MAX_RESULTS {
            break;
        }

        // 절대 임계값
        if result.relevance_score < MIN_RELEVANCE_SCORE {
            break;
        }

        // 점수 드롭 감지 (두 번째 결과부터)
        if let Some(prev) = filtered.last() {
            if prev.relevance_score - result.relevance_score > SCORE_DROP_THRESHOLD {
                break;
            }
        }

        filtered.push(result);
    }

    filtered
}

/// 벡터 검색 그룹핑을 위한 내부 구조체
struct DocumentGroup {
    summary: String,
    best_score: f32,
    matched_facts: Vec<String>,
}
