use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::Arc;

use crate::core::search::{BM25Scorer, SearchMode, reciprocal_rank_fusion};
use crate::llm::{create_llm_provider, create_embedding_provider, LlmProvider, EmbeddingProvider};
use crate::models::{Document, api::{IngestResponse, SearchResponse, SearchResult}};
use crate::settings::SettingsManager;
use crate::storage::{DocumentStore, QdrantStorage, SearchHit};

/// RRF 상수 (일반적으로 60 사용)
const RRF_K: f32 = 60.0;

/// 최소 유사도 임계값
const MIN_VECTOR_SCORE: f32 = 0.5;

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

    /// 자연어 입력 → 파싱 → 임베딩 → 저장
    pub async fn ingest(&self, raw_content: String) -> Result<IngestResponse> {
        // 1. LLM으로 파싱
        tracing::info!("Parsing content...");
        let llm = self.get_llm_provider().await?;
        let parsed = llm.parse(&raw_content).await?;

        // 2. 문서 생성
        let doc = Document::new(
            raw_content,
            parsed.summary.clone(),
            parsed.tags.clone(),
            parsed.entities.clone(),
        );

        // 3. 임베딩 생성 (summary 기반)
        tracing::info!("Generating embedding...");
        let embedder = self.get_embedding_provider().await?;
        let embedding = embedder.embed(&parsed.summary).await?;

        // 4. 파일 시스템에 원본 저장
        tracing::info!("Saving document...");
        self.documents.save(&doc).await?;

        // 5. Qdrant에 벡터 저장
        tracing::info!("Indexing to Qdrant...");
        self.qdrant.upsert(&doc, embedding).await?;

        tracing::info!("Ingested document: {}", doc.id);

        Ok(IngestResponse {
            id: doc.id,
            summary: doc.summary,
            tags: doc.tags,
            entities: doc.entities,
        })
    }

    /// Hybrid Search: 벡터 검색 + 키워드 검색 결합
    pub async fn search(
        &self,
        query: String,
        limit: usize,
        offset: usize,
        mode: Option<String>,
        tags_filter: Option<Vec<String>>,
    ) -> Result<SearchResponse> {
        let search_mode: SearchMode = mode
            .as_deref()
            .and_then(|m| m.parse().ok())
            .unwrap_or_default();

        tracing::info!("Search mode: {:?}, query: {}", search_mode, query);

        let (results, total) = match search_mode {
            SearchMode::Vector => self.vector_search(&query, limit + offset, tags_filter.clone()).await?,
            SearchMode::Keyword => self.keyword_search(&query, limit + offset, tags_filter.clone()).await?,
            SearchMode::Hybrid => self.hybrid_search(&query, limit + offset, tags_filter.clone()).await?,
        };

        // 페이지네이션 적용
        let paginated: Vec<_> = results
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect();

        let sources_used: Vec<_> = paginated.iter().map(|r| r.id).collect();

        Ok(SearchResponse {
            results: paginated,
            sources_used,
            total,
            mode: format!("{:?}", search_mode).to_lowercase(),
        })
    }

    /// 벡터 검색
    async fn vector_search(
        &self,
        query: &str,
        limit: usize,
        tags_filter: Option<Vec<String>>,
    ) -> Result<(Vec<SearchResult>, usize)> {
        let embedder = self.get_embedding_provider().await?;
        let query_embedding = embedder.embed(query).await?;

        let hits = self.qdrant.search(query_embedding, tags_filter, limit * 2).await?;

        let results: Vec<SearchResult> = hits
            .into_iter()
            .filter(|h| h.score >= MIN_VECTOR_SCORE)
            .take(limit)
            .map(|hit| SearchResult {
                id: hit.id,
                summary: hit.summary,
                tags: hit.tags,
                relevance_score: hit.score,
            })
            .collect();

        let total = results.len();
        Ok((results, total))
    }

    /// 키워드 검색 (BM25)
    async fn keyword_search(
        &self,
        query: &str,
        limit: usize,
        tags_filter: Option<Vec<String>>,
    ) -> Result<(Vec<SearchResult>, usize)> {
        // 모든 문서 가져오기
        let all_docs = self.qdrant.scroll_all(tags_filter).await?;

        if all_docs.is_empty() {
            return Ok((vec![], 0));
        }

        // BM25 인덱스 구축
        let mut scorer = BM25Scorer::new();
        let mut doc_map: HashMap<String, SearchHit> = HashMap::new();

        for doc in all_docs {
            let id_str = doc.id.to_string();
            // raw_content + summary + tags를 결합하여 검색
            let search_text = format!(
                "{} {} {}",
                doc.raw_content,
                doc.summary,
                doc.tags.join(" ")
            );
            scorer.add_document(&id_str, &search_text);
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
                    tags: doc.tags.clone(),
                    relevance_score: (score / max_score).min(1.0),
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
        tags_filter: Option<Vec<String>>,
    ) -> Result<(Vec<SearchResult>, usize)> {
        // 병렬로 두 검색 수행
        let (vector_results, keyword_results) = tokio::join!(
            self.vector_search(query, limit * 2, tags_filter.clone()),
            self.keyword_search(query, limit * 2, tags_filter)
        );

        let vector_results = vector_results.unwrap_or((vec![], 0)).0;
        let keyword_results = keyword_results.unwrap_or((vec![], 0)).0;

        // 문서 정보 맵 구축
        let mut doc_map: HashMap<String, SearchResult> = HashMap::new();
        for r in vector_results.iter().chain(keyword_results.iter()) {
            doc_map.entry(r.id.to_string()).or_insert_with(|| r.clone());
        }

        // RRF 결합
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

        // 최종 결과 구성
        let max_rrf_score = fused.first().map(|(_, s)| *s).unwrap_or(1.0).max(0.001);

        let results: Vec<SearchResult> = fused
            .into_iter()
            .take(limit)
            .filter_map(|(id, rrf_score)| {
                let doc = doc_map.get(&id)?;
                Some(SearchResult {
                    id: doc.id,
                    summary: doc.summary.clone(),
                    tags: doc.tags.clone(),
                    // RRF 스코어를 0-1 범위로 정규화
                    relevance_score: (rrf_score / max_rrf_score).min(1.0),
                })
            })
            .collect();

        Ok((results, total))
    }

    /// 문서 조회
    pub async fn get_document(&self, id: uuid::Uuid) -> Result<Document> {
        self.documents.load(id).await
    }

    /// 최근 문서 목록 (페이지네이션 지원)
    pub async fn recent(
        &self,
        limit: usize,
        offset: usize,
        tags_filter: Option<Vec<String>>,
    ) -> Result<(Vec<Document>, usize)> {
        let mut docs = self.documents.list_recent(1000).await?;

        // 태그 필터링
        if let Some(tags) = tags_filter {
            docs.retain(|doc| tags.iter().any(|t| doc.tags.contains(t)));
        }

        let total = docs.len();
        let paginated = docs.into_iter().skip(offset).take(limit).collect();

        Ok((paginated, total))
    }

    /// 문서 수정 (raw_content 변경 → 재파싱 + 재임베딩)
    pub async fn update(&self, id: uuid::Uuid, raw_content: String) -> Result<IngestResponse> {
        // 1. LLM으로 파싱
        tracing::info!("Re-parsing content for update...");
        let llm = self.get_llm_provider().await?;
        let parsed = llm.parse(&raw_content).await?;

        // 2. 기존 문서의 created_at 보존, updated_at 갱신
        let existing = self.documents.load(id).await?;
        let now = chrono::Utc::now();
        let doc = Document {
            id,
            raw_content,
            summary: parsed.summary.clone(),
            tags: parsed.tags.clone(),
            entities: parsed.entities.clone(),
            created_at: existing.created_at,
            updated_at: now,
        };

        // 3. 임베딩 생성
        tracing::info!("Re-generating embedding...");
        let embedder = self.get_embedding_provider().await?;
        let embedding = embedder.embed(&parsed.summary).await?;

        // 4. 파일 덮어쓰기
        tracing::info!("Saving updated document...");
        self.documents.save(&doc).await?;

        // 5. Qdrant 업데이트 (upsert)
        tracing::info!("Updating Qdrant index...");
        self.qdrant.upsert(&doc, embedding).await?;

        tracing::info!("Updated document: {}", doc.id);

        Ok(IngestResponse {
            id: doc.id,
            summary: doc.summary,
            tags: doc.tags,
            entities: doc.entities,
        })
    }

    /// 문서 삭제
    pub async fn delete(&self, id: uuid::Uuid) -> Result<()> {
        // 1. Qdrant에서 삭제
        tracing::info!("Deleting from Qdrant...");
        self.qdrant.delete(id).await?;

        // 2. 파일 삭제
        tracing::info!("Deleting document file...");
        self.documents.delete(id).await?;

        tracing::info!("Deleted document: {}", id);
        Ok(())
    }

    /// 파일 시스템의 모든 문서를 Qdrant에 재인덱싱
    pub async fn reindex_all(&self) -> Result<usize> {
        let docs = self.documents.list_recent(10000).await?;
        let total = docs.len();

        if total == 0 {
            return Ok(0);
        }

        let embedder = self.get_embedding_provider().await?;
        let mut indexed = 0;

        for doc in docs {
            match embedder.embed(&doc.summary).await {
                Ok(embedding) => {
                    if let Err(e) = self.qdrant.upsert(&doc, embedding).await {
                        tracing::error!("Failed to index {}: {}", doc.id, e);
                    } else {
                        indexed += 1;
                        tracing::info!("Reindexed {}/{}: {}", indexed, total, doc.id);
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

    /// 모든 고유 태그 목록 조회
    pub async fn get_all_tags(&self) -> Result<Vec<String>> {
        let docs = self.qdrant.scroll_all(None).await?;
        let mut tags: std::collections::HashSet<String> = std::collections::HashSet::new();

        for doc in docs {
            for tag in doc.tags {
                tags.insert(tag);
            }
        }

        let mut tags_vec: Vec<_> = tags.into_iter().collect();
        tags_vec.sort();
        Ok(tags_vec)
    }
}
