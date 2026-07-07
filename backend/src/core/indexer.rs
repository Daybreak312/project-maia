use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use uuid::Uuid;

use crate::connectors::{ConnectorIngest, ConnectorIngestMode, ConnectorItem, ItemOutcome};
use crate::core::search::{BM25Scorer, SearchMode, reciprocal_rank_fusion};
use crate::core::ingest_agent::{CandidateDoc, IngestAgent, IngestStrategy, DEFAULT_EDGE_WEIGHT};
use crate::core::search_agent::{
    expansion_score, DeepSearchParams, ExpandOrigin, SearchAgent, SearchBackend,
    DEFAULT_DEEP_SEARCH_MAX_RESULTS,
};
use crate::llm::{
    build_http_client, create_embedding_provider, create_llm_provider, CodexProvider,
    EmbeddingProvider, LlmProvider, LocalEmbeddingProvider, ProviderType,
};
use crate::models::{Document, DocumentSource, Edge, RelationType, api::{AgentSearchMeta, IngestResponse, IngestOutcome, SearchResponse, SearchResult}};
use crate::patrol::decay::apply_edge_decay;
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

    /// 현재 설정의 파싱 provider로 LLM Provider 생성
    async fn get_llm_provider(&self) -> Result<Box<dyn LlmProvider>> {
        let provider_type = self.settings.get().await.parsing_provider;
        self.llm_provider_for(provider_type).await
    }

    /// 지정한 provider 타입으로 LLM Provider를 조립한다(검증 엔드포인트에서도 재사용).
    ///
    /// codex는 단순 키가 아니라 토큰 저장소(SettingsManager) 컨텍스트를 **공유**해야
    /// 단일 플라이트 refresh가 성립하므로 전용 경로로 만든다. local은 파싱 불가.
    pub async fn llm_provider_for(
        &self,
        provider_type: ProviderType,
    ) -> Result<Box<dyn LlmProvider>> {
        match provider_type {
            ProviderType::Codex => Ok(Box::new(CodexProvider::new(
                self.settings.clone(),
                build_http_client(),
            ))),
            ProviderType::Local => {
                Err(anyhow!("local은 임베딩 전용입니다 (파싱 provider로 사용 불가)"))
            }
            key_based => {
                let api_key = self
                    .settings
                    .get_api_key(key_based)
                    .await
                    .ok_or_else(|| anyhow!("API key for {} is not configured", key_based))?;
                create_llm_provider(key_based, api_key)
            }
        }
    }

    /// 현재 설정의 임베딩 provider로 Embedding Provider 생성
    async fn get_embedding_provider(&self) -> Result<Box<dyn EmbeddingProvider>> {
        let provider_type = self.settings.get().await.embedding_provider;
        self.embedding_provider_for(provider_type).await
    }

    /// 지정한 provider 타입으로 Embedding Provider를 조립한다(검증 엔드포인트 재사용).
    ///
    /// 생성 시 provider의 임베딩 차원을 Qdrant 기대 차원과 동기화한다 — 이후 컬렉션
    /// 대조가 provider 전환(차원 변경)을 즉시 감지해 "reindex 필요" 에러를 낸다(FR4).
    pub async fn embedding_provider_for(
        &self,
        provider_type: ProviderType,
    ) -> Result<Box<dyn EmbeddingProvider>> {
        let provider: Box<dyn EmbeddingProvider> = match provider_type {
            ProviderType::Local => {
                Box::new(LocalEmbeddingProvider::new(self.settings.models_dir()))
            }
            ProviderType::Codex => {
                return Err(anyhow!("codex는 임베딩 provider로 사용할 수 없습니다"))
            }
            key_based => {
                let api_key = self
                    .settings
                    .get_api_key(key_based)
                    .await
                    .ok_or_else(|| anyhow!("API key for {} is not configured", key_based))?;
                create_embedding_provider(key_based, api_key)?
            }
        };
        self.qdrant.set_target_dim(provider.dimension() as u64).await;
        Ok(provider)
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
    /// **정보 유실 0 불변식**: 어느 단계에서 실패하든 입력이 사라지는 경로가 없다.
    /// 판단 전 실패(LLM 미설정/판단 자체 실패)는 `raw_fallback`으로, 전략 실행 중
    /// 흡수 가능한 실패(임베딩 장애/대상 load 실패 등)는 원문 raw 폴백으로 처리한다.
    /// 폴백은 `persist_raw_document`(판단·파싱·임베딩과 무관하게 원문을 SSoT에 선기록)를
    /// 통하므로, 프로바이더가 전면 장애여도 원문이 디스크에 남는다. 응답에 `fallback=true`.
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
        let label = decision.strategy.label();
        tracing::info!("Smart ingest 전략: {} — {}", label, reason);

        // 전략 실행. 실행 단계의 흡수 가능한 실패(임베딩 장애, Update 대상 load 실패 등)
        // 에도 원문이 사라지지 않도록, 실패하면 원문을 raw로 보존한다(정보 유실 0 최후
        // 방어선). 실행기에는 원문 클론을 넘겨 원본을 폴백용으로 살려둔다 — String 클론
        // 비용은 LLM 호출 비용에 비해 무시 가능하다.
        match self
            .execute_decision(
                llm.as_ref(),
                &decision.strategy,
                raw_content.clone(),
                &candidates,
                &reason,
                workspace_id,
            )
            .await
        {
            Ok(outcome) => Ok(outcome),
            Err(e) => {
                tracing::warn!("전략 '{}' 실행 실패, 원문 raw 폴백: {}", label, e);
                self.raw_fallback(
                    raw_content,
                    workspace_id,
                    format!("전략 '{label}' 실행 실패로 raw 저장: {e}"),
                )
                .await
            }
        }
    }

    /// 판단된 전략을 실행한다. 실패는 호출 측(smart_ingest)의 raw 폴백으로 흡수된다.
    ///
    /// **LLM 호출 상한**: New/Update는 관계 판단 1회, Split/Duplicate는 관계 판단 없이
    /// RELATED_TO 기본(문서 수 비례 무한 호출 금지 불변식).
    #[allow(clippy::too_many_arguments)]
    async fn execute_decision(
        &self,
        llm: &dyn LlmProvider,
        strategy: &IngestStrategy,
        raw_content: String,
        candidates: &[CandidateDoc],
        reason: &str,
        workspace_id: &str,
    ) -> Result<IngestOutcome> {
        match strategy {
            IngestStrategy::New => {
                let resp = self.ingest_to_workspace(raw_content, workspace_id).await?;
                let new_id = resp.id;
                let edges = self
                    .create_auto_edges(llm, workspace_id, new_id, &resp.summary, candidates, true)
                    .await;
                Ok(IngestOutcome::from_response(resp, "new", vec![new_id], edges, false, reason))
            }
            IngestStrategy::Update { target } => {
                let target = *target;
                // Update 대상 로드 실패 시 New로 안전 강등한다(파서의 환각 target 강등과
                // 대칭). 원문을 신규 저장해 정보 유실을 막는다 — 실행 단계에도 판단 단계와
                // 동일한 방어선을 둔다.
                let existing = match self.documents.load(target, workspace_id).await {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::warn!("Update 대상 {} 로드 실패, New로 강등: {}", target, e);
                        let resp = self.ingest_to_workspace(raw_content, workspace_id).await?;
                        let new_id = resp.id;
                        let edges = self
                            .create_auto_edges(llm, workspace_id, new_id, &resp.summary, candidates, true)
                            .await;
                        return Ok(IngestOutcome::from_response(
                            resp,
                            "new",
                            vec![new_id],
                            edges,
                            false,
                            format!("업데이트 대상 로드 실패로 신규 저장: {reason}"),
                        ));
                    }
                };

                // 병합 전략 = 상한 있는 append: 기존 원문에 새 원문을 이어 붙이되, 재파싱
                // 입력이 무한 성장하지 않도록 총 길이를 제한한다(전체 이력은 버전 보관됨).
                // update_in_workspace가 덮어쓰기 전에 이전 버전을 보관한다.
                let merged = merge_for_update(&existing.raw_content, &raw_content);
                let resp = self.update_in_workspace(target, merged, workspace_id).await?;
                let others: Vec<CandidateDoc> =
                    candidates.iter().filter(|c| c.id != target).cloned().collect();
                let edges = self
                    .create_auto_edges(llm, workspace_id, target, &resp.summary, &others, true)
                    .await;
                Ok(IngestOutcome::from_response(resp, "update", vec![target], edges, false, reason))
            }
            IngestStrategy::Split { segments } => {
                self.execute_split(llm, segments, raw_content, candidates, reason, workspace_id)
                    .await
            }
            IngestStrategy::Duplicate { of } => {
                let of = *of;
                // 중복이어도 원문은 보관한다(정보 유실 0). 자동 삭제/병합은 하지 않고
                // (Phase 5 Review Queue의 사람 몫), 원본과 엣지로만 연결해 중복을 추적한다.
                let resp = self.ingest_to_workspace(raw_content, workspace_id).await?;
                let new_id = resp.id;
                let dup_target = vec![CandidateDoc { id: of, summary: String::new() }];
                let edges = self
                    .create_auto_edges(llm, workspace_id, new_id, &resp.summary, &dup_target, false)
                    .await;
                Ok(IngestOutcome::from_response(resp, "duplicate", vec![new_id], edges, false, reason))
            }
        }
    }

    /// 분할 전략 실행 — best-effort. 각 세그먼트를 독립 저장하되, 하나라도 실패하면
    /// 원문 전체를 raw dead-letter로 보존해 미저장 세그먼트의 유실을 막는다(정보 유실 0).
    /// 부분 실패는 응답에 `fallback=true`와 사유로 관측 가능하게 남긴다.
    #[allow(clippy::too_many_arguments)]
    async fn execute_split(
        &self,
        llm: &dyn LlmProvider,
        segments: &[String],
        raw_content: String,
        candidates: &[CandidateDoc],
        reason: &str,
        workspace_id: &str,
    ) -> Result<IngestOutcome> {
        let mut document_ids: Vec<Uuid> = Vec::new();
        let mut total_edges = 0usize;
        let mut primary: Option<IngestResponse> = None;
        let mut failed = 0usize;

        for segment in segments {
            match self.ingest_to_workspace(segment.clone(), workspace_id).await {
                Ok(resp) => {
                    document_ids.push(resp.id);
                    // Split은 관계 판단 없이 RELATED_TO로 연결 (LLM 호출 수를 상수로 유지).
                    let edges = self
                        .create_auto_edges(llm, workspace_id, resp.id, &resp.summary, candidates, false)
                        .await;
                    total_edges += edges;
                    if primary.is_none() {
                        primary = Some(resp);
                    }
                }
                Err(e) => {
                    failed += 1;
                    tracing::warn!("분할 세그먼트 저장 실패(best-effort 계속): {}", e);
                }
            }
        }

        if failed == 0 {
            // 전 세그먼트 성공 — 원문 dead-letter가 필요 없다.
            let primary = primary.ok_or_else(|| anyhow!("split 세그먼트가 비어 있습니다"))?;
            return Ok(IngestOutcome::from_response(
                primary,
                "split",
                document_ids,
                total_edges,
                false,
                reason,
            ));
        }

        // 일부(또는 전부) 세그먼트 실패 — 원문 전체를 raw dead-letter로 보존한다.
        tracing::warn!("분할 세그먼트 {}개 실패 — 원문을 raw dead-letter로 보존", failed);
        let dead_letter = self.persist_raw_document(raw_content, workspace_id, None).await?;
        document_ids.push(dead_letter.id);
        let rep = primary.unwrap_or_else(|| IngestResponse {
            id: dead_letter.id,
            summary: dead_letter.summary.clone(),
            entities: dead_letter.entities.clone(),
            facts: dead_letter.facts.clone(),
        });
        Ok(IngestOutcome::from_response(
            rep,
            "split",
            document_ids,
            total_edges,
            true,
            format!("분할 중 {failed}개 세그먼트 실패로 원문을 raw 보존: {reason}"),
        ))
    }

    /// raw 저장 폴백 — 판단·파싱·임베딩과 무관하게 원문을 저장하고 fallback=true로 표시.
    async fn raw_fallback(
        &self,
        raw_content: String,
        workspace_id: &str,
        reason: String,
    ) -> Result<IngestOutcome> {
        let doc = self.persist_raw_document(raw_content, workspace_id, None).await?;
        let id = doc.id;
        Ok(IngestOutcome::from_response(
            IngestResponse {
                id,
                summary: doc.summary,
                entities: doc.entities,
                facts: doc.facts,
            },
            "raw",
            vec![id],
            0,
            true,
            reason,
        ))
    }

    /// **최후의 방어선** — 원문을 판단·파싱·임베딩과 무관하게 디스크(SSoT)에 먼저 기록.
    ///
    /// 기존 폴백은 `ingest_to_workspace`를 재사용해 LLM `parse`와 임베딩을 다시 호출했다
    /// — 폴백이라는 이름의 경로가 폴백해야 할 바로 그 서비스에 의존하는 모순이었다.
    /// 프로바이더 장애 시 폴백 자체가 실패해 입력이 소실됐다. 이 메서드는 그 의존을 끊는다:
    /// 1. LLM 없이 원문 앞부분으로 summary를 만들고 raw JSON을 **먼저** 저장한다
    ///    (외부 서비스 무관 — 이 저장만이 정보 유실 0의 hard requirement).
    /// 2. 임베딩이 가용하면 best-effort로 Qdrant에 인덱싱해 즉시 검색 가능하게 한다.
    ///    실패해도 raw JSON은 SSoT로 남아 다음 reindex에서 복원되므로 유실이 없다.
    async fn persist_raw_document(
        &self,
        raw_content: String,
        workspace_id: &str,
        source: Option<DocumentSource>,
    ) -> Result<Document> {
        let summary = fallback_summary(&raw_content);
        let mut doc = Document::new(raw_content, summary, Vec::new(), Vec::new());
        // 커넥터 유입이면 출처를 각인한다(중복 방지 키·provenance).
        doc.source = source;

        // SSoT 저장 — 외부 서비스에 의존하지 않는 유일한 필수 단계.
        self.documents.save(&doc, workspace_id).await?;

        // best-effort 인덱싱 — 실패는 흡수하되 침묵하지 않는다(reindex로 복원 가능).
        self.best_effort_index(&doc, workspace_id).await;

        Ok(doc)
    }

    /// raw 문서를 best-effort로 Qdrant에 인덱싱한다. 임베딩/Qdrant 실패는 warn만 하고
    /// 흡수한다 — raw JSON이 이미 저장됐으므로 다음 reindex에서 복원된다(정보 유실 0).
    async fn best_effort_index(&self, doc: &Document, workspace_id: &str) {
        let embedder = match self.get_embedding_provider().await {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("raw 저장 후 임베딩 provider 미확보(인덱싱 생략, reindex로 복원): {}", e);
                return;
            }
        };
        match self.build_chunks(embedder.as_ref(), &doc.summary, &doc.facts).await {
            Ok(chunks) => {
                if let Err(e) = self
                    .qdrant
                    .upsert_chunks(
                        workspace_id,
                        doc.id,
                        &doc.summary,
                        &doc.created_at.to_rfc3339(),
                        &doc.edges,
                        chunks,
                    )
                    .await
                {
                    tracing::warn!("raw 문서 Qdrant 인덱싱 실패(reindex로 복원 가능): {}", e);
                }
            }
            Err(e) => tracing::warn!("raw 문서 임베딩 실패(reindex로 복원 가능): {}", e),
        }
    }

    /// 입력과 의미적으로 관련된 기존 문서 후보를 찾는다(요약만, 유사도 상위 소수).
    async fn find_candidates(
        &self,
        content: &str,
        workspace_id: &str,
    ) -> Result<Vec<CandidateDoc>> {
        let embedder = self.get_embedding_provider().await?;
        // 후보 탐색은 쿼리 임베딩 경로(e5 계열은 query: 접두).
        let query_embedding = embedder.embed_query(content).await?;
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
            agent: None,
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
        // 검색 쿼리는 쿼리 임베딩 경로(e5 계열은 query: 접두 — 문서 인덱싱과 대칭).
        let query_embedding = embedder.embed_query(query).await?;

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
                expanded_from: None,
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
                    expanded_from: None,
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
        // 차원 불일치 사전 감지(FR4): 하이브리드는 아래에서 vector/keyword의 개별
        // 실패를 graceful하게 삼키므로(unwrap_or), 그대로 두면 차원 불일치가 "빈 결과"로
        // 침묵된다(기본 검색 모드가 hybrid이라 치명적). 임베딩 provider가 가용할 때만
        // 컬렉션 차원을 대조해 불일치면 "reindex 필요" 에러를 명시적으로 올린다. provider
        // 구성 실패(키 미설정 등 별개 문제)는 여기서 삼켜 기존 keyword 폴백을 보존한다.
        if self.get_embedding_provider().await.is_ok() {
            self.qdrant.ensure_collection(workspace_id).await?;
        }

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
                    expanded_from: None,
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
        // 1. LLM 파싱 + 임베딩 (계산 — 쓰기 락 밖에서 수행해 임계 구역을 짧게 유지).
        tracing::info!("Re-parsing content for update...");
        let llm = self.get_llm_provider().await?;
        let parsed = llm.parse(&raw_content).await?;
        tracing::info!("Re-generating embeddings...");
        let embedder = self.get_embedding_provider().await?;
        let chunks = self
            .build_chunks(embedder.as_ref(), &parsed.summary, &parsed.facts)
            .await?;

        // 2. 원자적 임계 구역: 최신 상태 재로드 → 버전 보관 → 병합 저장.
        //    edges는 문서의 raw JSON에 사는 그래프 상태이므로 재파싱 업데이트에서
        //    유실되면 안 된다(reindex 생존 불변식). LLM 파싱이 도는 동안 다른 라이터가
        //    엣지를 추가했을 수 있으므로, 파싱 전에 캡처한 스냅샷이 아니라 **write_lock
        //    아래에서 갓 로드한 최신 edges**를 보존해 lost-update를 막는다.
        let doc = {
            let _guard = self.documents.write_guard().await;
            let existing = self.documents.load(id, workspace_id).await?;

            // 덮어쓰기 전에 이전 상태를 버전으로 보관한다(잘못된 업데이트의 안전망).
            // 보관에 실패하면 이전 버전을 남길 수 없으므로 업데이트를 진행하지 않는다
            // — "이전 버전 보장" 시맨틱을 지켜 오염 위험을 차단한다.
            self.versions.archive(&existing, workspace_id).await?;

            let doc = Document {
                id,
                raw_content,
                summary: parsed.summary.clone(),
                entities: parsed.entities.clone(),
                facts: parsed.facts.clone(),
                // 최신 edges 보존(락 아래 재로드 → 동시 추가된 엣지도 포함).
                edges: existing.edges.clone(),
                // 출처는 edges와 마찬가지로 재파싱 업데이트에서 보존한다(커넥터 유입 문서의
                // provenance 유지). 커넥터 업데이트 경로는 이후 명시적으로 갱신한다.
                source: existing.source.clone(),
                created_at: existing.created_at,
                updated_at: chrono::Utc::now(),
            };

            // 파일 덮어쓰기 (락 아래 — 저장까지 원자적).
            tracing::info!("Saving updated document...");
            self.documents.save(&doc, workspace_id).await?;
            doc
        };

        // 3. 기존 chunk 전부 삭제 후 새 chunk 저장 (보존된 edges를 payload에 재비정규화, 락 밖)
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
    ///
    /// **쓰기 직렬화 불변식(부활 방지):** raw JSON(SSoT) 파일 제거를 `delete_serialized`로
    /// 수행해, 동시 실행되는 감쇠·엣지 추가(`DocumentStore::update`)의 load→save와 같은
    /// `write_lock`으로 직렬화한다. 이 직렬화가 없으면 update가 문서를 load한 뒤 save하기
    /// 전에 삭제가 파일을 지우고, 뒤늦은 save가 삭제된 문서를 raw JSON에 되살려(reindex로
    /// 검색 부활) SSoT와 파생 인덱스가 영구 불일치한다. Qdrant chunk 삭제(파생물)는 임계
    /// 구역 밖에 둬 네트워크 I/O로 모든 쓰기를 직렬화하지 않는다(파생물은 reindex로 복원).
    pub async fn delete_from_workspace(&self, id: uuid::Uuid, workspace_id: &str) -> Result<()> {
        // 1. Qdrant에서 해당 문서의 모든 chunk 삭제 (파생물 — 임계 구역 밖).
        tracing::info!("Deleting chunks from Qdrant...");
        self.qdrant.delete_by_document_id(workspace_id, id).await?;

        // 2. raw JSON(SSoT) 파일 삭제 — 쓰기 락 아래에서 update의 save와 직렬화(부활 차단).
        tracing::info!("Deleting document file...");
        self.documents.delete_serialized(id, workspace_id).await?;

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
        // 1. raw JSON을 원자적으로 로드→엣지 추가→저장 (SSoT 먼저).
        //    DocumentStore::update가 쓰기 락으로 직렬화해, 동시 감쇠/업데이트가 이
        //    엣지를 stale 스냅샷으로 덮어쓰는 lost-update를 원천 차단한다.
        let doc = self
            .documents
            .update(source_id, workspace_id, |doc| {
                doc.add_edge(edge);
                doc.updated_at = chrono::Utc::now();
                true
            })
            .await?
            .ok_or_else(|| anyhow!("엣지 추가 대상 문서 없음: {source_id}"))?;

        // 2. Qdrant summary payload의 edges 재동기화 (파생물 갱신, 락 밖).
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
        // 원자적 로드→제거→저장 (쓰기 락으로 직렬화). 제거할 엣지가 없으면 저장을 생략한다.
        let mut removed = 0usize;
        let updated = self
            .documents
            .update(source_id, workspace_id, |doc| {
                removed = doc.remove_edge(target_id);
                if removed > 0 {
                    doc.updated_at = chrono::Utc::now();
                    true
                } else {
                    false
                }
            })
            .await?
            .ok_or_else(|| anyhow!("엣지 제거 대상 문서 없음: {source_id}"))?;

        // 실제 제거가 있었을 때만 payload 재동기화 (파생물 갱신, 락 밖).
        if removed > 0 {
            self.qdrant
                .update_edges_payload(workspace_id, source_id, &updated.edges)
                .await?;
        }
        Ok(removed)
    }

    // ──── Patrol 지원 (Phase 5) ────

    /// 워크스페이스의 raw 문서 전체를 반환한다(Patrol 신호 수집용).
    /// list_recent를 큰 상한으로 재사용한다 — 개인 규모에서 전 문서를 담기 충분하다.
    pub async fn all_documents(&self, workspace_id: &str) -> Result<Vec<Document>> {
        self.documents.list_recent(usize::MAX, workspace_id).await
    }

    /// **복구 가능한 삭제** — 삭제 전에 현재 상태를 버전으로 보관한 뒤 삭제한다.
    ///
    /// Review Queue의 "삭제" 판단이 쓰는 경로다. 버전 스냅샷이 남아 잘못된 삭제도 복구할
    /// 수 있다(파괴 전 복구 가능성 보장). 이미 없는 문서는 성공으로 본다(멱등 — 판단
    /// 재제출/경합에서 이중 삭제가 에러가 되지 않게).
    pub async fn soft_delete_document(&self, workspace_id: &str, id: Uuid) -> Result<()> {
        if !self.documents.exists(id, workspace_id).await {
            return Ok(()); // 이미 삭제됨 — 멱등 성공
        }
        let doc = self.documents.load(id, workspace_id).await?;
        // 삭제 전 버전 보관(복구 가능성). 실패 시 삭제하지 않는다(안전망 우선).
        self.versions.archive(&doc, workspace_id).await?;
        self.delete_from_workspace(id, workspace_id).await
    }

    /// 워크스페이스 전 문서의 엣지에 **시간 감쇠**를 자동 재계산한다(수학적 유지보수).
    ///
    /// 가중치가 바뀐 문서만 raw JSON(SSoT)을 저장하고, Qdrant payload는 best-effort로
    /// 동기화한다(payload는 파생물 — 실패해도 reindex로 복원). 감쇠는 **문서 내용 변경이
    /// 아니므로 `updated_at`을 건드리지 않는다**(staleness 기준점이 리셋되면 안 됨).
    /// `lambda <= 0`이면 감쇠 없음(0 반환). 바뀐 엣지 총수를 반환한다.
    pub async fn decay_workspace_edges(
        &self,
        workspace_id: &str,
        lambda: f32,
        now: DateTime<Utc>,
    ) -> Result<usize> {
        if lambda <= 0.0 {
            return Ok(0);
        }
        // 문서 ID만 열거하고(벌크 스냅샷을 저장에 재사용하지 않는다), 감쇠는 문서별로
        // 쓰기 락 아래에서 **갓 로드된 최신 상태**에 적용한다. 기존 구현은 전 문서를 T0
        // 스냅샷으로 적재한 뒤 그 stale 스냅샷을 문서별로 blind overwrite했기에, 패스가
        // 도는 동안 커넥터/에이전트가 추가한 엣지가 조용히 소실됐다(SSoT 오염, reindex
        // 복원 불가). 이제 경합 창이 코퍼스 전체 패스에서 단일 문서의 원자적 load→save로
        // 좁혀지고, 그 창마저 write_lock이 직렬화해 lost-update가 사라진다.
        let ids: Vec<Uuid> = self
            .documents
            .list_recent(usize::MAX, workspace_id)
            .await?
            .into_iter()
            .map(|d| d.id)
            .collect();

        let mut total_changed = 0usize;
        for id in ids {
            let mut changed = 0usize;
            // 원자적 로드→감쇠→저장. updated_at은 의도적으로 보존(staleness 기준점 유지).
            // 열거 후 삭제된 문서는 Ok(None) — 조용히 건너뛴다.
            let updated = self
                .documents
                .update(id, workspace_id, |doc| {
                    changed = apply_edge_decay(doc, now, lambda);
                    changed > 0
                })
                .await?;
            let Some(doc) = updated else { continue };
            if changed == 0 {
                continue;
            }
            // payload 동기화는 best-effort — 실패해도 raw가 진실이라 reindex로 복원된다.
            if let Err(e) = self
                .qdrant
                .update_edges_payload(workspace_id, doc.id, &doc.edges)
                .await
            {
                tracing::warn!("엣지 감쇠 payload 동기화 실패 {}(reindex로 복원): {}", doc.id, e);
            }
            total_changed += changed;
        }
        Ok(total_changed)
    }

    // ──── 커넥터 유입 (Phase 4) ────

    /// 커넥터 신규 항목을 Parsed 모드로 유입한다 — LLM 파싱 + 임베딩 + 출처 각인 +
    /// 그래프 자동 연결(RELATED_TO, LLM 관계 판단 없이 상수 호출).
    ///
    /// **실패 시맨틱**: 파싱/임베딩 *계산* 실패는 `Err`(항목 실패 → 러너가 재시도).
    /// Qdrant *쓰기* 실패는 best-effort로 흡수한다 — raw JSON이 SSoT로 저장됐으므로
    /// reindex로 인덱스를 복원할 수 있다(정보 유실 0). raw 저장 자체가 실패하면 `Err`.
    async fn ingest_connector_new_parsed(
        &self,
        content: String,
        source: DocumentSource,
        workspace_id: &str,
    ) -> Result<Uuid> {
        let llm = self.get_llm_provider().await?;
        let parsed = llm.parse(&content).await?;
        let embedder = self.get_embedding_provider().await?;
        // 임베딩(계산)은 raw 저장 전에 끝낸다 — 여기서 실패하면 아무것도 안 바뀌어 재시도가 안전하다.
        let chunks = self
            .build_chunks(embedder.as_ref(), &parsed.summary, &parsed.facts)
            .await?;
        // 그래프 연결 후보는 원문 임베딩으로 확보한다(content 이동 전).
        let candidates = self.find_candidates(&content, workspace_id).await.unwrap_or_default();

        let doc = Document::new(
            content,
            parsed.summary.clone(),
            parsed.entities.clone(),
            parsed.facts.clone(),
        )
        .with_source(source);
        let id = doc.id;

        // raw JSON(SSoT) 저장 — 실패하면 Err(정보 유실 방지).
        self.documents.save(&doc, workspace_id).await?;

        // Qdrant 인덱싱은 best-effort — 실패해도 raw는 남아 reindex로 복원된다.
        if let Err(e) = self
            .qdrant
            .upsert_chunks(
                workspace_id,
                id,
                &doc.summary,
                &doc.created_at.to_rfc3339(),
                &doc.edges,
                chunks,
            )
            .await
        {
            tracing::warn!("커넥터 문서 {} Qdrant 인덱싱 실패(raw 저장됨, reindex로 복원): {}", id, e);
        }

        // 자동 엣지 (RELATED_TO, judge 없이 — 대량 유입에서 LLM 호출 상수 유지).
        self.create_auto_edges(llm.as_ref(), workspace_id, id, &doc.summary, &candidates, false)
            .await;

        Ok(id)
    }

    /// 기존(동일 소스) 문서를 새 원문으로 갱신한다 — 내용 교체(append 아님), 출처 갱신,
    /// edges·created_at 보존. 파일은 자기 콘텐츠의 SSoT이므로 커넥터 업데이트는 병합이
    /// 아니라 교체다.
    ///
    /// **실패 시맨틱**: Parsed의 파싱/임베딩 계산 실패는 저장 전에 `Err`(재시도 안전).
    /// 이전 버전은 덮어쓰기 전에 archive된다.
    async fn update_connector_source(
        &self,
        id: Uuid,
        content: String,
        source: DocumentSource,
        mode: ConnectorIngestMode,
        workspace_id: &str,
    ) -> Result<()> {
        // Parsed는 파싱·임베딩을 저장 전(그리고 쓰기 락 밖)에 끝낸다 — 실패해도 문서
        // 미변경(재시도 안전)이고, 임계 구역을 계산으로 늘리지 않는다.
        let (summary, entities, facts, chunks) = match mode {
            ConnectorIngestMode::Parsed => {
                let llm = self.get_llm_provider().await?;
                let parsed = llm.parse(&content).await?;
                let embedder = self.get_embedding_provider().await?;
                let chunks = self
                    .build_chunks(embedder.as_ref(), &parsed.summary, &parsed.facts)
                    .await?;
                (parsed.summary, parsed.entities, parsed.facts, Some(chunks))
            }
            ConnectorIngestMode::Raw => (fallback_summary(&content), Vec::new(), Vec::new(), None),
        };

        // 원자적 임계 구역: 최신 상태 재로드 → 버전 보관 → 교체 저장. 파싱이 도는 동안
        // 다른 라이터가 추가한 엣지를 stale 스냅샷으로 덮어쓰지 않도록, write_lock 아래에서
        // 갓 로드한 최신 edges를 보존한다(그래프 엣지는 재유입 업데이트에서 유실 금지).
        let doc = {
            let _guard = self.documents.write_guard().await;
            let existing = self.documents.load(id, workspace_id).await?;

            // 덮어쓰기 전에 이전 버전 보관(안전망). 실패 시 업데이트 중단.
            self.versions.archive(&existing, workspace_id).await?;

            let doc = Document {
                id,
                raw_content: content,
                summary: summary.clone(),
                entities,
                facts,
                // 최신 edges 보존(락 아래 재로드 → 동시 추가된 엣지도 포함).
                edges: existing.edges.clone(),
                // 출처를 새 수정 시각으로 갱신한다(다음 스캔의 "변경 없음" 판단 기준).
                source: Some(source),
                created_at: existing.created_at,
                updated_at: chrono::Utc::now(),
            };

            self.documents.save(&doc, workspace_id).await?;
            doc
        };

        // Qdrant 재동기화: 기존 chunk 삭제 후 새로 upsert (best-effort — raw는 SSoT).
        if let Err(e) = self.qdrant.delete_by_document_id(workspace_id, id).await {
            tracing::warn!("커넥터 업데이트 {} 기존 chunk 삭제 실패(계속): {}", id, e);
        }
        match chunks {
            Some(chunks) => {
                if let Err(e) = self
                    .qdrant
                    .upsert_chunks(
                        workspace_id,
                        id,
                        &doc.summary,
                        &doc.created_at.to_rfc3339(),
                        &doc.edges,
                        chunks,
                    )
                    .await
                {
                    tracing::warn!("커넥터 업데이트 {} Qdrant 인덱싱 실패(reindex로 복원): {}", id, e);
                }
            }
            // Raw 모드는 폴백 요약을 best-effort 인덱싱.
            None => self.best_effort_index(&doc, workspace_id).await,
        }

        Ok(())
    }

    /// 커넥터 항목 하나를 유입한다 — 소스 식별자 기반 중복 방지의 핵심 결정 지점.
    ///
    /// 1. `(source_type, source_id)` 일치 문서 조회.
    /// 2. 있고 원본이 더 새롭지 않으면 → `Skipped`(재유입 안전성).
    /// 3. 있고 더 새로우면 → 업데이트 경로(`Updated`) — 신규 문서 난립 금지.
    /// 4. 없으면 → 신규(`Created`).
    async fn ingest_connector_item_inner(
        &self,
        workspace_id: &str,
        source_type: &str,
        connector_id: &str,
        item: ConnectorItem,
        mode: ConnectorIngestMode,
    ) -> Result<ItemOutcome> {
        let existing = self
            .documents
            .find_by_source(source_type, &item.source_id, workspace_id)
            .await?;

        if let Some(existing) = existing {
            // 변경 없음(원본이 더 새롭지 않음) → 재유입하지 않는다.
            if let Some(src) = &existing.source {
                if src.modified_at >= item.modified_at {
                    return Ok(ItemOutcome::Skipped);
                }
            }
            // 변경됨 → 업데이트 경로.
            let source = item.to_source(source_type, connector_id);
            self.update_connector_source(existing.id, item.content, source, mode, workspace_id)
                .await?;
            return Ok(ItemOutcome::Updated(existing.id));
        }

        // 신규 유입.
        let source = item.to_source(source_type, connector_id);
        let id = match mode {
            ConnectorIngestMode::Parsed => {
                self.ingest_connector_new_parsed(item.content, source, workspace_id)
                    .await?
            }
            ConnectorIngestMode::Raw => {
                self.persist_raw_document(item.content, workspace_id, Some(source))
                    .await?
                    .id
            }
        };
        Ok(ItemOutcome::Created(id))
    }

    /// 파일 시스템의 모든 문서를 Qdrant에 재인덱싱
    /// 컬렉션을 완전히 재생성하여 구 스키마 데이터도 정리
    pub async fn reindex_all(&self) -> Result<usize> {
        self.reindex_workspace(DEFAULT_WORKSPACE).await
    }

    /// 특정 워크스페이스의 Qdrant 컬렉션을 재인덱싱한다.
    ///
    /// **차원 마이그레이션(FR4):** embedder를 먼저 확보해 Qdrant 기대 차원을 현재
    /// provider 차원으로 동기화한 뒤 컬렉션을 재생성한다. 따라서 gemini(3072)→
    /// local(384) 전환 후 이 한 번의 reindex로 컬렉션이 새 차원으로 재생성되고
    /// raw JSON(SSoT) 전량이 재임베딩된다. 문서 0건이어도 컬렉션은 새 차원으로
    /// 재생성해, 이후 유입이 차원 불일치로 실패하지 않게 한다.
    pub async fn reindex_workspace(&self, workspace_id: &str) -> Result<usize> {
        // 상한 없이 raw JSON(SSoT) **전량**을 적재한다. list_recent는 created_at 내림차순
        // 정렬 후 truncate하므로, 유한 상한을 두면 컬렉션을 drop한 뒤 가장 **오래된** 문서
        // (인공두뇌의 초기 기반 기억)를 검색 인덱스에서 소리 없이 절단한다 — "문서 수 손실 0"
        // AC와 "침묵 금지" 원칙 정면 위반. 자매 함수 all_documents / decay_workspace_edges도
        // 동일하게 usize::MAX를 쓴다(개인 규모에서 전 문서를 담기 충분).
        let docs = self.documents.list_recent(usize::MAX, workspace_id).await?;
        let total = docs.len();

        // embedder를 먼저 확보 — 이 호출이 Qdrant 기대 차원을 현재 provider로 맞춘다.
        let embedder = self.get_embedding_provider().await?;

        // 컬렉션 재생성 (구 스키마/구 차원 orphan 정리 → 현재 차원으로 생성).
        self.qdrant.recreate_collection(workspace_id).await?;

        if total == 0 {
            return Ok(0);
        }

        let mut indexed = 0;
        let mut skipped = 0; // 스냅샷 이후 삭제되어 부활을 막은 문서 수(관측용)

        for doc in docs {
            // summary + facts 임베딩 (facts가 비어있으면 summary chunk만)
            match self.build_chunks(embedder.as_ref(), &doc.summary, &doc.facts).await {
                Ok(chunks) => {
                    let chunk_count = chunks.len();

                    // **부활 방지(SSoT 재확인) — reindex도 "삭제=삭제" 불변식을 지킨다.**
                    // `docs`는 T0 스냅샷이라, reindex 루프(로컬 임베딩은 문서별 직렬 → 수 분)가
                    // 도는 동안 소유자/Review Queue가 이 문서를 삭제(delete_from_workspace →
                    // raw JSON SSoT 제거)했을 수 있다. stale 스냅샷을 그대로 upsert하면 소유자가
                    // 의도적으로 파기한 지식이 새 컬렉션에 **부활**한다 — search는 반환하나
                    // get_document는 404(SSoT/파생 인덱스 영구 불일치). delete가 raw 제거를
                    // write_lock으로 직렬화하므로 exists=false는 삭제 확정을 뜻한다. upsert 직전에
                    // 재확인해 **지금 살아있는 문서만** 인덱싱하고, 동시 유입이 남긴 chunk가 있으면
                    // delete_by_document_id로 현재 SSoT 기준 reconcile한다. 침묵 금지: skip을
                    // warn으로 관측 신호화한다. (재확인~upsert 사이 극소 창에 삭제가 끼면 stale이
                    // 남을 수 있으나, raw는 SSoT로 온전하고 재-reindex로 복원된다 — 정보 유실 0 유지.)
                    if !self.documents.exists(doc.id, workspace_id).await {
                        skipped += 1;
                        if let Err(e) = self.qdrant.delete_by_document_id(workspace_id, doc.id).await {
                            tracing::error!("Failed to reconcile deleted {}: {}", doc.id, e);
                        }
                        tracing::warn!("Skipped reindex of {} — 스냅샷 이후 삭제됨(부활 방지)", doc.id);
                        continue;
                    }

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

        if skipped > 0 {
            tracing::warn!(
                "Reindex complete: {}/{} indexed, {} skipped (스냅샷 이후 삭제 — 부활 방지)",
                indexed, total, skipped
            );
        } else {
            tracing::info!("Reindex complete: {}/{} documents", indexed, total);
        }
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
            agent: None,
        })
    }

    /// agent(deep) 검색 — Search Agent가 충분성 평가·쿼리 재작성·그래프 확장으로
    /// 능동 탐색한다. 자기 자신을 `SearchBackend`로 주입해 실제 검색·확장 I/O를 제공하고,
    /// LLM provider는 있으면 판단에 사용하되 없으면(미설정) 에이전트가 폴백한다.
    ///
    /// **에러를 삼키지 않되 결과로 귀결**: LLM 실패·미설정은 폴백(초기 결과 + 표시)으로
    /// 흡수되어 항상 `SearchResponse`를 반환한다(agent 메타데이터에 폴백 사유 명시).
    ///
    /// `workspace_ids`는 호출 전에 접근 권한·존재 여부로 이미 필터링되어 있어야 한다
    /// (교차 워크스페이스 조합 가능 — Phase 1 기능 위에서 동작).
    pub async fn deep_search_across_workspaces(
        &self,
        query: String,
        workspace_ids: &[String],
        params: DeepSearchParams,
    ) -> Result<SearchResponse> {
        // LLM provider는 판단용 — 미확보 시 None으로 넘겨 에이전트가 폴백을 처리한다.
        let llm = self.get_llm_provider().await.ok();
        let agent = SearchAgent::new();

        let outcome = agent
            .deep_search(self, llm.as_deref(), &query, workspace_ids, params)
            .await;

        let sources_used: Vec<Uuid> = outcome.results.iter().map(|r| r.id).collect();
        let total = outcome.results.len();

        Ok(SearchResponse {
            results: outcome.results,
            sources_used,
            total,
            mode: "agent".to_string(),
            agent: Some(AgentSearchMeta {
                rounds: outcome.rounds,
                queries: outcome.queries,
                graph_expanded: outcome.graph_expanded,
                expansion_count: outcome.expansion_count,
                fallback: outcome.fallback,
                reason: outcome.reason,
            }),
        })
    }

}

/// Indexer를 Patrol의 문서 실행기로 노출한다(전 문서 조회·복구 가능 삭제·엣지 감쇠).
/// 러너/스케줄러 패턴과 동일하게, 단위 테스트는 이 trait을 mock으로 대체해 Qdrant·LLM
/// 없이 Patrol 오케스트레이션(신호 수집·탐지·enqueue·판단·감쇠)을 검증한다.
#[async_trait]
impl crate::patrol::PatrolExecutor for Indexer {
    async fn all_documents(&self, workspace_id: &str) -> Result<Vec<Document>> {
        Indexer::all_documents(self, workspace_id).await
    }
    async fn soft_delete_document(&self, workspace_id: &str, id: Uuid) -> Result<()> {
        Indexer::soft_delete_document(self, workspace_id, id).await
    }
    async fn decay_workspace_edges(
        &self,
        workspace_id: &str,
        lambda: f32,
        now: DateTime<Utc>,
    ) -> Result<usize> {
        Indexer::decay_workspace_edges(self, workspace_id, lambda, now).await
    }
}

/// Indexer를 Search Agent의 검색 백엔드로 노출한다. 에이전트 파이프라인이 Qdrant·
/// DocumentStore에 직접 의존하지 않고 이 얇은 어댑터를 통해서만 I/O한다.
#[async_trait]
impl SearchBackend for Indexer {
    /// 한 라운드 검색 — 기존 교차 워크스페이스 hybrid 검색 파이프라인을 그대로 재사용한다
    /// (관련성 필터링·점수 부여가 이미 끝난 결과를 반환).
    async fn run_search(&self, query: &str, workspaces: &[String]) -> Result<Vec<SearchResult>> {
        let resp = self
            .search_across_workspaces(
                query.to_string(),
                DEFAULT_DEEP_SEARCH_MAX_RESULTS,
                0,
                None, // 기본 모드(hybrid)
                workspaces,
                TimeSearchOptions::default(),
            )
            .await?;
        Ok(resp.results)
    }

    /// 출처 문서들의 그래프 이웃을 확장한다. 각 출처를 그 워크스페이스에서 개별
    /// 탐색해 유래(`expanded_from`)를 명확히 하고, 점수는 출처 점수와 엣지 가중치에서
    /// 파생한다. 한 출처의 이웃 조회 실패는 전체를 막지 않는다(best-effort).
    async fn expand(&self, origins: &[ExpandOrigin], depth: usize) -> Result<Vec<SearchResult>> {
        let mut out: Vec<SearchResult> = Vec::new();
        for origin in origins {
            let nodes = match self
                .documents
                .neighbors(origin.id, depth, &origin.workspace)
                .await
            {
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!("그래프 확장 이웃 조회 실패({}): {}", origin.id, e);
                    continue;
                }
            };
            for node in nodes {
                out.push(SearchResult {
                    id: node.document.id,
                    summary: node.document.summary.clone(),
                    relevance_score: expansion_score(origin.score, node.weight),
                    workspace: origin.workspace.clone(),
                    matched_facts: Vec::new(),
                    created_at: Some(node.document.created_at),
                    expanded_from: Some(origin.id),
                });
            }
        }
        Ok(out)
    }
}

/// 커넥터 유입 실행기 — 러너/스케줄러가 이 trait 경유로 Indexer의 파싱·임베딩·저장
/// 파이프라인을 재사용한다(에이전트는 판단만, 저장은 기존 인덱싱 재사용 원칙).
///
/// `Box<dyn Connector>`/`Arc<dyn ConnectorIngest>`로 쓰이므로 `#[async_trait]`가 필요하다
/// (네이티브 AFIT는 dyn 비호환).
#[async_trait]
impl ConnectorIngest for Indexer {
    async fn ingest_item(
        &self,
        workspace_id: &str,
        source_type: &str,
        connector_id: &str,
        item: ConnectorItem,
        mode: ConnectorIngestMode,
    ) -> Result<ItemOutcome> {
        self.ingest_connector_item_inner(workspace_id, source_type, connector_id, item, mode)
            .await
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

/// LLM 파싱 없이 원문에서 요약을 만든다(최후 방어선용).
///
/// 원문 앞부분을 잘라 쓰되 문자 경계를 지켜(멀티바이트 안전) 최대 `MAX`자로 제한하고,
/// 잘린 경우 말줄임표를 붙여 부분 요약임을 표시한다. 순수 함수 — mock 없이 테스트된다.
fn fallback_summary(raw: &str) -> String {
    const MAX: usize = 200;
    let trimmed = raw.trim();
    let truncated: String = trimmed.chars().take(MAX).collect();
    if trimmed.chars().count() > MAX {
        format!("{truncated}…")
    } else {
        truncated
    }
}

/// 업데이트 병합 — 기존 원문에 새 원문을 이어 붙이되, 재파싱 입력이 무한 성장하지
/// 않도록 총 길이에 상한을 둔다(모든 루프에 상한 불변식).
///
/// 상한 초과 시 **새 입력은 항상 온전히 보존**하고 기존 원문은 뒤쪽(최근)부터 남긴다.
/// 잘려나간 이력 전체는 VersionStore 스냅샷에 남으므로 정보 유실이 없다. 이 상한이
/// 없으면 반복 업데이트로 raw_content가 선형 성장해 결국 LLM 컨텍스트를 초과, 그 문서가
/// 영구히 업데이트 불가가 되고 이후 입력이 소실된다. 순수 함수 — mock 없이 테스트된다.
fn merge_for_update(existing_raw: &str, new_raw: &str) -> String {
    const MAX_MERGED_CHARS: usize = 20_000;
    const SEP: &str = "\n\n---\n\n";

    let existing_len = existing_raw.chars().count();
    let new_len = new_raw.chars().count();
    let sep_len = SEP.chars().count();

    if existing_len + sep_len + new_len <= MAX_MERGED_CHARS {
        return format!("{existing_raw}{SEP}{new_raw}");
    }

    // 상한 초과: 새 입력 + 구분자 예산을 확보하고 기존 원문은 최근 tail만 남긴다.
    let budget = MAX_MERGED_CHARS.saturating_sub(new_len + sep_len);
    if budget == 0 {
        // 새 입력만으로 이미 상한 이상 — 새 입력(최신 기억)은 절대 잃지 않으므로 그대로.
        return new_raw.to_string();
    }
    let existing_chars: Vec<char> = existing_raw.chars().collect();
    let keep_from = existing_chars.len().saturating_sub(budget);
    let kept: String = existing_chars[keep_from..].iter().collect();
    format!("{kept}{SEP}{new_raw}")
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
            expanded_from: None,
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

    // ──── 최후 방어선: fallback_summary (LLM 없는 요약) ────

    #[test]
    fn test_fallback_summary_short_input_trimmed_passthrough() {
        // 짧은 입력은 트림 후 그대로 (말줄임표 없음).
        assert_eq!(fallback_summary("  짧은 메모  "), "짧은 메모");
    }

    #[test]
    fn test_fallback_summary_truncates_long_input() {
        // 200자를 넘으면 200자로 자르고 말줄임표를 붙인다.
        let long = "가".repeat(500);
        let s = fallback_summary(&long);
        assert_eq!(s.chars().count(), 201, "200자 + 말줄임표");
        assert!(s.ends_with('…'));
    }

    #[test]
    fn test_fallback_summary_multibyte_safe() {
        // 멀티바이트 경계에서 잘려도 패닉 없이 유효한 문자열이어야 한다(바이트 슬라이싱 금지).
        let emoji = "🧠".repeat(300);
        let s = fallback_summary(&emoji);
        assert!(s.chars().count() <= 201);
        assert!(s.starts_with('🧠'));
    }

    #[test]
    fn test_fallback_summary_empty_input() {
        assert_eq!(fallback_summary("   "), "");
    }

    // ──── Update 병합 상한: merge_for_update (무한 성장 방지) ────

    #[test]
    fn test_merge_for_update_under_cap_appends_full() {
        // 상한 이하면 기존+새 원문을 구분자로 온전히 이어 붙인다.
        let m = merge_for_update("기존 내용", "새 내용");
        assert!(m.contains("기존 내용"));
        assert!(m.contains("새 내용"));
        assert!(m.contains("---"), "구분자로 이어 붙여야 한다");
    }

    #[test]
    fn test_merge_for_update_caps_growth_keeps_new_whole() {
        // 기존 원문이 상한을 넘길 만큼 크면, 새 입력은 온전히 보존하고 기존은 최근만 남긴다.
        let existing = "A".repeat(30_000);
        let new = "새로운 최신 입력";
        let m = merge_for_update(&existing, new);
        assert!(m.chars().count() <= 20_000, "총 길이가 상한 이하여야 한다");
        assert!(m.ends_with(new), "새 입력이 끝에 온전히 보존되어야 한다");
    }

    #[test]
    fn test_merge_for_update_repeated_updates_stay_bounded() {
        // 반복 업데이트로 raw_content가 무한 성장하지 않아야 한다(시한폭탄 방지 불변식).
        let mut acc = String::from("초기");
        for i in 0..100 {
            acc = merge_for_update(&acc, &"업데이트 ".repeat(50));
            assert!(acc.chars().count() <= 20_000, "누적이 상한을 넘으면 안 된다 (회차 {i})");
        }
    }

    #[test]
    fn test_merge_for_update_huge_new_input_preserved() {
        // 새 입력 자체가 상한보다 커도 통째로 보존한다(최신 기억 우선).
        let new = "B".repeat(25_000);
        let m = merge_for_update("기존", &new);
        assert!(m.contains(&new), "상한보다 큰 새 입력도 온전히 보존");
    }
}
