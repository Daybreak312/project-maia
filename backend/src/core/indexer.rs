use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use uuid::Uuid;

use crate::core::search::{BM25Scorer, SearchMode, reciprocal_rank_fusion};
use crate::core::ingest_agent::{CandidateDoc, IngestAgent, IngestStrategy, DEFAULT_EDGE_WEIGHT};
use crate::llm::{create_llm_provider, create_embedding_provider, LlmProvider, EmbeddingProvider};
use crate::models::{Document, Edge, RelationType, api::{IngestResponse, IngestOutcome, SearchResponse, SearchResult}};
use crate::settings::SettingsManager;
use crate::storage::{DocumentStore, QdrantStorage, SearchHit, ChunkData, VersionStore};

/// smart ingest 시 판단에 참고할 최대 후보 문서 수 (프롬프트 비용·지연 억제).
const MAX_INGEST_CANDIDATES: usize = 5;

/// 관련 후보로 채택할 최소 유사도 (이 미만은 판단·엣지 후보에서 제외).
const CANDIDATE_MIN_SCORE: f32 = 0.5;

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
    versions: Arc<VersionStore>,
}

impl Indexer {
    pub fn new(
        settings: Arc<SettingsManager>,
        qdrant: Arc<QdrantStorage>,
        documents: Arc<DocumentStore>,
        versions: Arc<VersionStore>,
    ) -> Self {
        Self {
            settings,
            qdrant,
            documents,
            versions,
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

        // 5. Qdrant에 chunk 벡터 저장 (신규 문서라 edges는 비어 있음)
        tracing::info!("Indexing {} chunks to Qdrant (workspace: {})...", chunks.len(), workspace_id);
        self.qdrant.upsert_chunks(
            workspace_id,
            doc.id,
            &doc.summary,
            &doc.created_at.to_rfc3339(),
            &doc.edges,
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

    /// Smart Ingest — 저장 전에 에이전트가 신규/업데이트/분할/중복을 판단하고 실행한다.
    ///
    /// **정보 유실 0 불변식**: 어느 단계든(LLM 미설정/후보 검색 실패/판단 실패) 실패하면
    /// raw 저장으로 폴백하며 응답에 `fallback=true`로 표시한다. 저장 실행은 기존 인덱싱
    /// 파이프라인(`ingest_to_workspace`/`update_in_workspace`)을 실행기로 재사용한다.
    ///
    /// **LLM 호출 상한**: New/Update는 판단 1 + 관계 판단 1 = 2회. Split은 판단 1회
    /// (세그먼트 수 비례 관계 판단을 피해 기본 RELATED_TO로 연결). Duplicate는 판단 1회.
    pub async fn smart_ingest_to_workspace(
        &self,
        raw_content: String,
        workspace_id: &str,
    ) -> Result<IngestOutcome> {
        let agent = IngestAgent::new();

        // 1. LLM provider 확보 — 실패 시 raw 폴백 (판단 자체가 불가)
        let llm = match self.get_llm_provider().await {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("LLM provider 미확보, raw 폴백: {}", e);
                return self
                    .raw_fallback(raw_content, workspace_id, format!("LLM 미설정으로 raw 저장: {e}"))
                    .await;
            }
        };

        // 2. 관련 후보 검색 (실패해도 빈 후보로 판단 계속 — 최소한 New는 가능)
        let candidates = match self.find_candidates(&raw_content, workspace_id).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("후보 검색 실패, 후보 없이 진행: {}", e);
                Vec::new()
            }
        };

        // 3. 전략 판단 — 실패 시 raw 폴백
        let decision = match agent.decide(llm.as_ref(), &raw_content, &candidates).await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("에이전트 판단 실패, raw 폴백: {}", e);
                return self
                    .raw_fallback(
                        raw_content,
                        workspace_id,
                        format!("에이전트 판단 실패로 raw 저장: {e}"),
                    )
                    .await;
            }
        };

        let reason = decision.reason.clone();
        tracing::info!("Smart ingest 전략: {} — {}", decision.strategy.label(), reason);

        match decision.strategy {
            IngestStrategy::New => {
                let resp = self.ingest_to_workspace(raw_content, workspace_id).await?;
                let new_id = resp.id;
                let edges = self
                    .create_auto_edges(llm.as_ref(), workspace_id, new_id, &resp.summary, &candidates, true)
                    .await;
                Ok(IngestOutcome::from_response(resp, "new", vec![new_id], edges, false, reason))
            }
            IngestStrategy::Update { target } => {
                // 병합 전략 = append: 기존 원문에 새 원문을 이어 붙여 재파싱한다(정보 누적).
                // update_in_workspace가 덮어쓰기 전에 이전 버전을 보관한다.
                let existing = self.documents.load(target, workspace_id).await?;
                let merged = format!("{}\n\n---\n\n{}", existing.raw_content, raw_content);
                let resp = self.update_in_workspace(target, merged, workspace_id).await?;
                let others: Vec<CandidateDoc> =
                    candidates.iter().filter(|c| c.id != target).cloned().collect();
                let edges = self
                    .create_auto_edges(llm.as_ref(), workspace_id, target, &resp.summary, &others, true)
                    .await;
                Ok(IngestOutcome::from_response(resp, "update", vec![target], edges, false, reason))
            }
            IngestStrategy::Split { segments } => {
                let mut document_ids = Vec::new();
                let mut total_edges = 0;
                let mut primary: Option<IngestResponse> = None;
                for segment in segments {
                    let resp = self.ingest_to_workspace(segment, workspace_id).await?;
                    let seg_id = resp.id;
                    document_ids.push(seg_id);
                    // Split은 관계 판단 없이 RELATED_TO로 연결 (LLM 호출 수를 상수로 유지).
                    let edges = self
                        .create_auto_edges(llm.as_ref(), workspace_id, seg_id, &resp.summary, &candidates, false)
                        .await;
                    total_edges += edges;
                    if primary.is_none() {
                        primary = Some(resp);
                    }
                }
                let primary = primary.ok_or_else(|| anyhow!("split 세그먼트가 비어 있습니다"))?;
                Ok(IngestOutcome::from_response(
                    primary,
                    "split",
                    document_ids,
                    total_edges,
                    false,
                    reason,
                ))
            }
            IngestStrategy::Duplicate { of } => {
                // 중복이어도 원문은 보관한다(정보 유실 0). 자동 삭제/병합은 하지 않고
                // (Phase 5 Review Queue의 사람 몫), 원본과 엣지로만 연결해 중복을 추적한다.
                let resp = self.ingest_to_workspace(raw_content, workspace_id).await?;
                let new_id = resp.id;
                let dup_target = vec![CandidateDoc {
                    id: of,
                    summary: String::new(),
                }];
                let edges = self
                    .create_auto_edges(llm.as_ref(), workspace_id, new_id, &resp.summary, &dup_target, false)
                    .await;
                Ok(IngestOutcome::from_response(
                    resp,
                    "duplicate",
                    vec![new_id],
                    edges,
                    false,
                    reason,
                ))
            }
        }
    }

    /// raw 저장 폴백 — 판단을 우회하고 기존 파이프라인으로 저장한 뒤 fallback=true로 표시.
    async fn raw_fallback(
        &self,
        raw_content: String,
        workspace_id: &str,
        reason: String,
    ) -> Result<IngestOutcome> {
        let resp = self.ingest_to_workspace(raw_content, workspace_id).await?;
        let id = resp.id;
        Ok(IngestOutcome::from_response(resp, "raw", vec![id], 0, true, reason))
    }

    /// 입력과 의미적으로 관련된 기존 문서 후보를 찾는다(요약만, 유사도 상위 소수).
    async fn find_candidates(
        &self,
        content: &str,
        workspace_id: &str,
    ) -> Result<Vec<CandidateDoc>> {
        let embedder = self.get_embedding_provider().await?;
        let query_embedding = embedder.embed(content).await?;
        // 같은 문서의 여러 chunk가 히트할 수 있어 넉넉히 over-fetch 후 document 단위로 dedup.
        let hits = self
            .qdrant
            .search(workspace_id, query_embedding, MAX_INGEST_CANDIDATES * 4)
            .await?;

        let mut seen: HashSet<Uuid> = HashSet::new();
        let mut candidates = Vec::new();
        for hit in hits {
            if hit.score < CANDIDATE_MIN_SCORE {
                continue;
            }
            if seen.insert(hit.id) {
                candidates.push(CandidateDoc {
                    id: hit.id,
                    summary: hit.summary,
                });
                if candidates.len() >= MAX_INGEST_CANDIDATES {
                    break;
                }
            }
        }
        Ok(candidates)
    }

    /// 출발 문서에서 후보 문서들로 자동 엣지를 생성한다. 생성된 엣지 수를 반환.
    ///
    /// `judge=true`면 관계 타입을 LLM으로 판단하고, `false`면 RELATED_TO 기본을 쓴다.
    /// 개별 엣지 저장 실패는 침묵하지 않되(warn) 전체를 중단시키지 않는다(best-effort).
    async fn create_auto_edges(
        &self,
        llm: &dyn LlmProvider,
        workspace_id: &str,
        source_id: Uuid,
        source_summary: &str,
        candidates: &[CandidateDoc],
        judge: bool,
    ) -> usize {
        let targets: Vec<CandidateDoc> = candidates
            .iter()
            .filter(|c| c.id != source_id)
            .cloned()
            .collect();
        if targets.is_empty() {
            return 0;
        }

        let relations: Vec<(Uuid, RelationType)> = if judge {
            IngestAgent::new()
                .judge_relations(llm, source_summary, &targets)
                .await
        } else {
            targets
                .iter()
                .map(|t| (t.id, RelationType::RelatedTo))
                .collect()
        };

        let mut created = 0;
        for (target_id, relation) in relations {
            let edge = Edge::new(target_id, relation, DEFAULT_EDGE_WEIGHT);
            match self
                .add_edge_to_document(workspace_id, source_id, edge)
                .await
            {
                Ok(_) => created += 1,
                Err(e) => {
                    tracing::warn!("자동 엣지 생성 실패 {}→{}: {}", source_id, target_id, e)
                }
            }
        }
        created
    }

    /// Hybrid Search: 벡터 검색 + 키워드 검색 결합 (시간 인식 없음, 기본)
    pub async fn search(
        &self,
        query: String,
        limit: usize,
        offset: usize,
        mode: Option<String>,
    ) -> Result<SearchResponse> {
        self.search_in_workspace(query, limit, offset, mode, DEFAULT_WORKSPACE, TimeSearchOptions::default())
            .await
    }

    /// 특정 워크스페이스에서 검색한다.
    ///
    /// 시간 인식 처리 순서: 기간 필터(created_at) → 관련성 필터(원본 cosine) →
    /// 시간 감쇠 재정렬(opt-in). 감쇠는 순위만 바꾸고 표시 점수는 원본 유사도를
    /// 유지하므로, 오래됐지만 매우 관련있는 문서가 임계값에서 사라지지 않는다.
    pub async fn search_in_workspace(
        &self,
        query: String,
        limit: usize,
        offset: usize,
        mode: Option<String>,
        workspace_id: &str,
        time: TimeSearchOptions,
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

        // 1. 기간 필터 (created_at 기준)
        let results = apply_period_filter(results, time.since, time.until);

        // 2. 관련성 기반 동적 필터링 (원본 cosine 기준)
        let filtered = filter_by_relevance(results);

        // 3. 시간 감쇠 재정렬 (opt-in) — 관련성 통과 결과의 순위만 최신 우선으로 조정
        let filtered = if time.decay {
            apply_time_decay(filtered, Utc::now(), time.lambda)
        } else {
            filtered
        };
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
                created_at: hit.created_at,
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
                workspace: workspace_id.to_string(),
                matched_facts: group.matched_facts,
                created_at: group.created_at,
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
                    workspace: workspace_id.to_string(),
                    matched_facts: vec![],
                    created_at: doc.created_at,
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
                    workspace: workspace_id.to_string(),
                    matched_facts: doc.matched_facts.clone(),
                    created_at: doc.created_at,
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
        self.recent_in_workspace(limit, offset, DEFAULT_WORKSPACE).await
    }

    /// 특정 워크스페이스의 최근 문서 목록 (페이지네이션 지원)
    pub async fn recent_in_workspace(
        &self,
        limit: usize,
        offset: usize,
        workspace_id: &str,
    ) -> Result<(Vec<Document>, usize)> {
        let docs = self.documents.list_recent(1000, workspace_id).await?;
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

        // 2. 기존 문서의 created_at·edges 보존, updated_at 갱신.
        //    edges는 문서의 raw JSON에 사는 그래프 상태이므로 재파싱 업데이트에서
        //    유실되면 안 된다(reindex 생존 불변식과 동일한 이유).
        let existing = self.documents.load(id, workspace_id).await?;

        // 덮어쓰기 전에 이전 상태를 버전으로 보관한다(잘못된 업데이트의 안전망).
        // 보관에 실패하면 이전 버전을 남길 수 없으므로 업데이트를 진행하지 않는다
        // — "이전 버전 보장" 시맨틱을 지켜 오염 위험을 차단한다.
        self.versions.archive(&existing, workspace_id).await?;

        let now = chrono::Utc::now();
        let doc = Document {
            id,
            raw_content,
            summary: parsed.summary.clone(),
            entities: parsed.entities.clone(),
            facts: parsed.facts.clone(),
            edges: existing.edges.clone(),
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

        // 5. 기존 chunk 전부 삭제 후 새 chunk 저장 (보존된 edges를 payload에 재비정규화)
        tracing::info!("Updating Qdrant index...");
        self.qdrant.delete_by_document_id(workspace_id, id).await?;
        self.qdrant.upsert_chunks(
            workspace_id,
            id,
            &doc.summary,
            &doc.created_at.to_rfc3339(),
            &doc.edges,
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

    /// 문서에 엣지를 추가한다.
    ///
    /// **동기화 순서 불변식:** raw JSON을 먼저 갱신하고(SSoT), 성공한 뒤에만 Qdrant
    /// summary payload를 재동기화한다. raw 저장이 실패하면 Qdrant는 호출조차 되지
    /// 않아 "둘 다 미반영"이 관측된다. payload 동기화만 실패하는 경우 raw와 순간
    /// 불일치하나, payload는 파생물이므로 reindex로 언제든 복원된다.
    pub async fn add_edge_to_document(
        &self,
        workspace_id: &str,
        source_id: Uuid,
        edge: Edge,
    ) -> Result<Document> {
        // 1. raw JSON 로드 → 엣지 추가 → 저장 (SSoT 먼저)
        let mut doc = self.documents.load(source_id, workspace_id).await?;
        doc.add_edge(edge);
        doc.updated_at = chrono::Utc::now();
        self.documents.save(&doc, workspace_id).await?;

        // 2. Qdrant summary payload의 edges 재동기화 (파생물 갱신)
        self.qdrant
            .update_edges_payload(workspace_id, source_id, &doc.edges)
            .await?;

        Ok(doc)
    }

    /// 시작 문서의 그래프 이웃을 depth 상한과 함께 조회한다(순환 안전).
    pub async fn neighbors_in_workspace(
        &self,
        start_id: Uuid,
        depth: usize,
        workspace_id: &str,
    ) -> Result<Vec<crate::storage::NeighborNode>> {
        self.documents.neighbors(start_id, depth, workspace_id).await
    }

    /// 문서에서 특정 대상 문서로 향하는 엣지를 모두 제거한다. 제거된 개수를 반환한다.
    /// 제거할 엣지가 없으면 저장/동기화를 생략한다(불필요한 쓰기 억제).
    pub async fn remove_edge_from_document(
        &self,
        workspace_id: &str,
        source_id: Uuid,
        target_id: Uuid,
    ) -> Result<usize> {
        let mut doc = self.documents.load(source_id, workspace_id).await?;
        let removed = doc.remove_edge(target_id);
        if removed > 0 {
            doc.updated_at = chrono::Utc::now();
            self.documents.save(&doc, workspace_id).await?;
            self.qdrant
                .update_edges_payload(workspace_id, source_id, &doc.edges)
                .await?;
        }
        Ok(removed)
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
                    // reindex 엣지 생존의 핵심: raw JSON의 edges를 payload로 복원한다.
                    if let Err(e) = self.qdrant.upsert_chunks(
                        workspace_id,
                        doc.id,
                        &doc.summary,
                        &doc.created_at.to_rfc3339(),
                        &doc.edges,
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

    /// 워크스페이스 생성 시 Qdrant 컬렉션을 준비한다.
    /// Qdrant 불가용 시 실패할 수 있으나, 호출 측에서 best-effort로 처리한다
    /// (컬렉션은 최초 ingest 시 lazy 하게도 보장됨).
    pub async fn provision_workspace_collection(&self, workspace_id: &str) -> Result<()> {
        self.qdrant.create_workspace_collection(workspace_id).await
    }

    /// 워크스페이스 삭제 시 Qdrant 컬렉션을 정리한다.
    pub async fn purge_workspace_collection(&self, workspace_id: &str) -> Result<()> {
        self.qdrant.delete_workspace_collection(workspace_id).await
    }

    /// 교차 워크스페이스 검색.
    ///
    /// 주어진 워크스페이스들 각각에서 hybrid 검색을 수행한 뒤(각 결과에 출처
    /// 워크스페이스가 스탬프됨), relevance_score 기준으로 병합·재정렬한다.
    /// 각 워크스페이스 검색은 이미 내부적으로 RRF 융합을 마쳐 raw cosine 점수를
    /// 부여하며, 동일 임베딩 공간이므로 워크스페이스 간 점수 비교가 유효하다.
    ///
    /// `workspace_ids`는 호출 전에 접근 권한·존재 여부로 이미 필터링되어 있어야 한다.
    pub async fn search_across_workspaces(
        &self,
        query: String,
        limit: usize,
        offset: usize,
        mode: Option<String>,
        workspace_ids: &[String],
        time: TimeSearchOptions,
    ) -> Result<SearchResponse> {
        // 단일 대상이면 일반 검색으로 위임 (불필요한 오케스트레이션 회피)
        if workspace_ids.len() == 1 {
            return self
                .search_in_workspace(query, limit, offset, mode, &workspace_ids[0], time)
                .await;
        }

        let mode_label = mode
            .as_deref()
            .and_then(|m| m.parse::<SearchMode>().ok())
            .unwrap_or_default();

        // 각 워크스페이스에서 상위 (limit+offset)개 후보를 수집한다.
        // (각 워크스페이스 검색에서 기간 필터는 적용되지만 감쇠 재정렬은 최종 병합 후
        //  한 번에 하므로, 개별 검색에는 감쇠를 끄고 병합 결과에 적용한다.)
        let per_ws_time = TimeSearchOptions {
            decay: false,
            ..time.clone()
        };
        let mut merged: Vec<SearchResult> = Vec::new();
        for ws in workspace_ids {
            match self
                .search_in_workspace(query.clone(), limit + offset, 0, mode.clone(), ws, per_ws_time.clone())
                .await
            {
                Ok(resp) => merged.extend(resp.results),
                Err(e) => {
                    // 부분 실패는 침묵하지 않되, 전체 검색을 중단시키지 않는다.
                    tracing::warn!("Cross-workspace search failed for '{}': {}", ws, e);
                }
            }
        }

        // 병합 결과 재정렬: 감쇠가 켜져 있으면 시간 감쇠 순위로, 아니면 relevance 내림차순.
        // (동일 임베딩 공간이라 워크스페이스 간 점수 비교가 유효)
        if time.decay {
            merged = apply_time_decay(merged, Utc::now(), time.lambda);
        } else {
            merged.sort_by(|a, b| {
                b.relevance_score
                    .partial_cmp(&a.relevance_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        let total = merged.len();
        let paginated: Vec<_> = merged.into_iter().skip(offset).take(limit).collect();
        let sources_used: Vec<_> = paginated.iter().map(|r| r.id).collect();

        Ok(SearchResponse {
            results: paginated,
            sources_used,
            total,
            mode: format!("{:?}", mode_label).to_lowercase(),
        })
    }

}

/// 교차 검색 대상 워크스페이스 집합을 계산한다.
///
/// (primary + 워크스페이스 설정의 cross_workspace 목록) 중에서
/// - 인증 컨텍스트가 접근 가능하고 (`can_access_workspace`)
/// - 실제로 존재하는(`existing`)
/// 워크스페이스만 남긴다. primary는 항상 첫 번째에 위치하며 중복은 제거된다.
///
/// 순수 함수 — 단위 테스트로 격리 검증한다.
pub fn cross_workspace_targets(
    primary: &str,
    cross_list: &[String],
    ctx: &crate::auth::AuthContext,
    existing: &std::collections::HashSet<String>,
) -> Vec<String> {
    let mut targets: Vec<String> = Vec::new();

    // primary를 맨 앞에 두고, 이어서 cross_list를 순회한다.
    for ws in std::iter::once(primary).chain(cross_list.iter().map(|s| s.as_str())) {
        if targets.iter().any(|t| t == ws) {
            continue; // 중복 제거
        }
        if existing.contains(ws) && ctx.can_access_workspace(ws) {
            targets.push(ws.to_string());
        }
    }

    targets
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

/// 시간 인식 검색 옵션.
#[derive(Debug, Clone, Default)]
pub struct TimeSearchOptions {
    /// 시간 감쇠 재정렬 적용 여부 (opt-in).
    pub decay: bool,
    /// 감쇠 강도 lambda (exp(-lambda*age_days)). decay=false면 무시.
    pub lambda: f32,
    /// 기간 필터 시작 (이 시각 포함 이후).
    pub since: Option<DateTime<Utc>>,
    /// 기간 필터 끝 (이 시각 포함 이전).
    pub until: Option<DateTime<Utc>>,
}

/// 시간 감쇠 계수: `exp(-lambda * age_days)`.
///
/// lambda ≤ 0이면 1.0(감쇠 없음). 미래 시각(age < 0)은 age=0으로 clamp한다.
/// age가 커질수록 0에 수렴하므로 최신 문서일수록 계수가 크다.
pub fn time_decay_factor(created_at: DateTime<Utc>, now: DateTime<Utc>, lambda: f32) -> f32 {
    if lambda <= 0.0 {
        return 1.0;
    }
    let age_secs = (now - created_at).num_seconds().max(0) as f32;
    let age_days = age_secs / 86_400.0;
    (-lambda * age_days).exp()
}

/// created_at 기준 기간 필터를 적용한다.
///
/// since/until이 모두 없으면 그대로 반환. 필터가 지정되면 created_at이 없는 결과는
/// 기간 만족 여부를 판단할 수 없으므로 보수적으로 제외한다.
pub fn apply_period_filter(
    results: Vec<SearchResult>,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
) -> Vec<SearchResult> {
    if since.is_none() && until.is_none() {
        return results;
    }
    results
        .into_iter()
        .filter(|r| match r.created_at {
            Some(ts) => since.is_none_or(|s| ts >= s) && until.is_none_or(|u| ts <= u),
            None => false,
        })
        .collect()
}

/// 시간 감쇠로 결과를 재정렬한다.
///
/// 표시 점수(relevance_score)는 원본 유사도를 유지하고, 정렬 순서만 감쇠 점수
/// (relevance × decay_factor)로 결정한다 — "동일 유사도면 최신 우선". created_at이
/// 없는 결과는 감쇠 계수 1.0으로 취급한다(감쇠에서 제외).
pub fn apply_time_decay(
    mut results: Vec<SearchResult>,
    now: DateTime<Utc>,
    lambda: f32,
) -> Vec<SearchResult> {
    results.sort_by(|a, b| {
        let sa = decayed_score(a, now, lambda);
        let sb = decayed_score(b, now, lambda);
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });
    results
}

fn decayed_score(r: &SearchResult, now: DateTime<Utc>, lambda: f32) -> f32 {
    match r.created_at {
        Some(ts) => r.relevance_score * time_decay_factor(ts, now, lambda),
        None => r.relevance_score,
    }
}

/// 벡터 검색 그룹핑을 위한 내부 구조체
struct DocumentGroup {
    summary: String,
    best_score: f32,
    matched_facts: Vec<String>,
    created_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{AuthContext, Permission};
    use crate::models::api::SearchResult;
    use chrono::Duration;
    use std::collections::HashSet;

    fn existing(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    fn cross(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    /// 특정 워크스페이스만 접근 가능한 (마스터 아님) 키 컨텍스트
    fn scoped(workspaces: &[&str]) -> AuthContext {
        AuthContext {
            key_id: "test".to_string(),
            permissions: Permission::ReadWrite,
            workspaces: workspaces.iter().map(|s| s.to_string()).collect(),
            is_master: false,
        }
    }

    #[test]
    fn test_targets_primary_only_when_no_cross() {
        let ctx = scoped(&["personal"]);
        let targets =
            cross_workspace_targets("personal", &[], &ctx, &existing(&["personal", "work"]));
        assert_eq!(targets, vec!["personal".to_string()]);
    }

    #[test]
    fn test_targets_includes_allowed_cross() {
        let ctx = scoped(&["personal", "work"]);
        let targets = cross_workspace_targets(
            "personal",
            &cross(&["work"]),
            &ctx,
            &existing(&["personal", "work"]),
        );
        // primary 우선, 이어서 허용된 교차 워크스페이스
        assert_eq!(targets, vec!["personal".to_string(), "work".to_string()]);
    }

    #[test]
    fn test_targets_excludes_inaccessible_cross() {
        // 키가 personal만 접근 가능 — 설정에 work가 있어도 제외되어야 한다 (스코프 교집합)
        let ctx = scoped(&["personal"]);
        let targets = cross_workspace_targets(
            "personal",
            &cross(&["work"]),
            &ctx,
            &existing(&["personal", "work"]),
        );
        assert_eq!(targets, vec!["personal".to_string()]);
        assert!(!targets.contains(&"work".to_string()), "접근 불가 워크스페이스는 교차 대상에서 제외");
    }

    #[test]
    fn test_targets_excludes_nonexistent_cross() {
        let ctx = scoped(&["personal", "ghost"]);
        let targets = cross_workspace_targets(
            "personal",
            &cross(&["ghost"]),
            &ctx,
            &existing(&["personal"]), // ghost는 존재하지 않음
        );
        assert_eq!(targets, vec!["personal".to_string()]);
    }

    #[test]
    fn test_targets_master_includes_all_configured_existing() {
        let ctx = AuthContext::master();
        let targets = cross_workspace_targets(
            "personal",
            &cross(&["work", "archive"]),
            &ctx,
            &existing(&["personal", "work", "archive"]),
        );
        assert_eq!(
            targets,
            vec!["personal".to_string(), "work".to_string(), "archive".to_string()]
        );
    }

    #[test]
    fn test_targets_dedup_primary_in_cross_list() {
        let ctx = scoped(&["personal", "work"]);
        let targets = cross_workspace_targets(
            "personal",
            &cross(&["personal", "work"]), // primary가 목록에 중복 포함
            &ctx,
            &existing(&["personal", "work"]),
        );
        assert_eq!(targets, vec!["personal".to_string(), "work".to_string()]);
    }

    // ──── 시간 인식 검색 (감쇠·기간 필터) ────

    fn result_at(score: f32, created_at: Option<DateTime<Utc>>) -> SearchResult {
        SearchResult {
            id: Uuid::new_v4(),
            summary: String::new(),
            relevance_score: score,
            workspace: "default".to_string(),
            matched_facts: vec![],
            created_at,
        }
    }

    #[test]
    fn test_time_decay_factor_no_lambda_is_one() {
        let now = Utc::now();
        assert_eq!(time_decay_factor(now - Duration::days(100), now, 0.0), 1.0);
    }

    #[test]
    fn test_time_decay_factor_recent_higher_than_old() {
        let now = Utc::now();
        let recent = time_decay_factor(now - Duration::days(1), now, 0.1);
        let old = time_decay_factor(now - Duration::days(30), now, 0.1);
        assert!(recent > old, "최신일수록 감쇠 계수가 커야 한다");
        assert!(recent <= 1.0 && old > 0.0);
    }

    #[test]
    fn test_time_decay_factor_future_clamped_to_one() {
        let now = Utc::now();
        let f = time_decay_factor(now + Duration::days(5), now, 0.1);
        assert!((f - 1.0).abs() < 1e-6, "미래 시각은 1.0으로 clamp");
    }

    #[test]
    fn test_apply_period_filter_no_bounds_passthrough() {
        let out = apply_period_filter(vec![result_at(0.9, Some(Utc::now()))], None, None);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn test_apply_period_filter_since() {
        let now = Utc::now();
        let old = result_at(0.9, Some(now - Duration::days(10)));
        let recent = result_at(0.9, Some(now - Duration::days(1)));
        let out = apply_period_filter(vec![old, recent], Some(now - Duration::days(5)), None);
        assert_eq!(out.len(), 1, "since 이후 문서만 남아야 한다");
    }

    #[test]
    fn test_apply_period_filter_until() {
        let now = Utc::now();
        let old = result_at(0.9, Some(now - Duration::days(10)));
        let recent = result_at(0.9, Some(now - Duration::days(1)));
        let out = apply_period_filter(vec![old, recent], None, Some(now - Duration::days(5)));
        assert_eq!(out.len(), 1, "until 이전 문서만 남아야 한다");
    }

    #[test]
    fn test_apply_period_filter_excludes_missing_created_at() {
        let out = apply_period_filter(
            vec![result_at(0.9, None)],
            Some(Utc::now() - Duration::days(1)),
            None,
        );
        assert!(out.is_empty(), "created_at 없는 결과는 기간 필터 시 제외");
    }

    #[test]
    fn test_apply_time_decay_recent_first_on_equal_score() {
        // 동일 유사도, 다른 시각 → 최신이 상위 (PRD 인수 조건).
        let now = Utc::now();
        let old = result_at(0.8, Some(now - Duration::days(365)));
        let old_id = old.id;
        let recent = result_at(0.8, Some(now - Duration::days(1)));
        let recent_id = recent.id;
        let out = apply_time_decay(vec![old, recent], now, 0.1);
        assert_eq!(out[0].id, recent_id, "최신 문서가 상위여야 한다");
        assert_eq!(out[1].id, old_id);
    }

    #[test]
    fn test_apply_time_decay_preserves_display_score() {
        // 표시 점수(relevance_score)는 감쇠하지 않고 원본 유사도를 유지한다.
        let now = Utc::now();
        let out = apply_time_decay(vec![result_at(0.75, Some(now - Duration::days(10)))], now, 0.1);
        assert!((out[0].relevance_score - 0.75).abs() < f32::EPSILON);
    }

    #[test]
    fn test_apply_time_decay_missing_created_at_uses_raw_score() {
        // created_at이 없으면 감쇠 없이 원본 점수로 정렬 (높은 점수 상위).
        let now = Utc::now();
        let high = result_at(0.9, None);
        let high_id = high.id;
        let low = result_at(0.5, None);
        let out = apply_time_decay(vec![low, high], now, 0.1);
        assert_eq!(out[0].id, high_id);
    }
}
