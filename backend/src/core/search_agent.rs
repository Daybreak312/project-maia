//! Search Agent — 검색을 1회성 조회에서, 충분해질 때까지 스스로 각도를 바꿔
//! 탐색하는 능동 프로세스로 바꾸는 LLM 에이전트 (Maia의 "회상(recall)" 능력).
//!
//! 이 모듈은 **판단(충분성 평가·쿼리 재작성)과 합성(중복 제거·재정렬·절사)** 로직을
//! 담고, 실제 I/O(하이브리드 검색 라운드·그래프 이웃 확장)는 `SearchBackend` trait
//! 뒤로 주입받는다. 덕분에 파이프라인 전체를 Qdrant/실 LLM 없이 mock으로 단위
//! 테스트한다(각 분기: 충분/부족/재작성/확장/합성/폴백).
//!
//! 설계 원칙 (Ingest Agent와 대칭):
//! - **폴백 필수**: LLM 판단이 실패하거나 미설정이면 초기 hybrid 검색 결과를 그대로
//!   반환한다(에러가 아니라 결과 + `fallback=true` 표시). 인공두뇌 제1원칙 — 덜
//!   똑똑한 건 괜찮지만 회상 자체가 에러로 끊기면 안 된다.
//! - **상한**: 재작성 라운드 상한, LLM 호출 상한, 파이프라인 시간 상한, 결과 총량
//!   상한을 모두 강제한다. 동일 쿼리 재검색은 금지한다(루프 종료 조건).
//! - **보수적 조기 종료**: 충분성 평가는 애매하면 "충분"으로 판단해 과잉 탐색을 피한다.
//! - **테스트 용이성**: 프롬프트 빌더·응답 파서·합성 함수를 순수 함수로 분리한다.

use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use uuid::Uuid;

use crate::llm::{extract_json, LlmProvider};
use crate::models::api::SearchResult;

/// 쿼리 재작성 최대 라운드 수 (PRD: 최대 3회).
pub const MAX_REWRITE_ROUNDS: usize = 3;

/// 입력당 LLM 호출 상한 (PRD: 충분성 평가 + 재작성 합쳐 5회 이하).
pub const MAX_AGENT_LLM_CALLS: usize = 5;

/// 그래프 확장의 출처로 삼을 상위 결과 수 (확장 fan-out 억제).
const EXPANSION_ORIGIN_COUNT: usize = 3;

/// agent 검색이 반환하는 결과 총량 기본 상한 (확장이 결과를 무한히 불리지 않도록).
pub const DEFAULT_DEEP_SEARCH_MAX_RESULTS: usize = 20;

/// 그래프 확장 이웃의 점수 감쇠 계수. 확장으로 끌어온 문서는 직접 매칭보다
/// 약하게 취급되도록 origin 점수에 곱해진다(엣지 가중치와 함께).
const EXPANSION_SCORE_DISCOUNT: f32 = 0.9;

// ──────────────────────────────────────────────────────────────
// 백엔드 추상화 (I/O 주입 seam) — 실제 구현은 Indexer, 테스트는 mock.
// ──────────────────────────────────────────────────────────────

/// 그래프 확장의 출처 — 검색 결과 문서와 그 워크스페이스·점수.
#[derive(Debug, Clone)]
pub struct ExpandOrigin {
    pub id: Uuid,
    pub workspace: String,
    pub score: f32,
}

/// Search Agent가 의존하는 검색 백엔드. 한 라운드 검색과 그래프 이웃 확장을
/// 추상화해, 에이전트 파이프라인이 Qdrant/DocumentStore에 직접 의존하지 않게 한다.
#[async_trait]
pub trait SearchBackend: Send + Sync {
    /// 주어진 쿼리로 한 라운드 검색을 수행한다. 반환 결과는 이미 관련성 필터링과
    /// 점수 부여가 끝나 있어야 한다(기존 hybrid 검색 파이프라인 재사용).
    async fn run_search(&self, query: &str, workspaces: &[String]) -> Result<Vec<SearchResult>>;

    /// 출처 문서들의 그래프 이웃을 depth 상한으로 확장한다. 반환 결과는
    /// `expanded_from = Some(origin)`으로 유래가 표시되고, 점수는 origin 점수와
    /// 엣지 가중치에서 파생된다(`expansion_score`).
    async fn expand(&self, origins: &[ExpandOrigin], depth: usize) -> Result<Vec<SearchResult>>;
}

// ──────────────────────────────────────────────────────────────
// 에이전트 판단 결과 타입
// ──────────────────────────────────────────────────────────────

/// 충분성 평가 결과.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sufficiency {
    /// 결과가 충분 — 탐색 종료.
    Sufficient,
    /// 부족 — 쿼리 재작성 후 재탐색 필요.
    Insufficient,
}

/// deep search의 튜닝 파라미터 (워크스페이스 설정에서 유래).
#[derive(Debug, Clone)]
pub struct DeepSearchParams {
    /// 그래프 이웃 확장 깊이 (0이면 확장 생략).
    pub expansion_depth: usize,
    /// 전체 파이프라인 시간 상한 (초과 시 그 시점까지의 결과 반환).
    pub time_limit: Duration,
    /// 결과 총량 상한 (합성 시 상위 N 절사).
    pub max_results: usize,
}

/// deep search 파이프라인의 최종 산출물 — 결과 + 탐색 과정 메타데이터.
#[derive(Debug, Clone)]
pub struct DeepSearchOutcome {
    /// 합성(중복 제거·재정렬·절사)된 최종 결과.
    pub results: Vec<SearchResult>,
    /// 수행된 검색 라운드 수 (초기 1회 + 재작성 재검색 횟수).
    pub rounds: usize,
    /// 실제 시도된 쿼리 목록 (원 쿼리가 항상 첫 번째).
    pub queries: Vec<String>,
    /// 그래프 이웃 확장이 이웃을 실제로 반환했는지 여부.
    pub graph_expanded: bool,
    /// 최종 결과 중 그래프 확장 유래(expanded_from Some)로 남은 문서 수.
    pub expansion_count: usize,
    /// LLM 판단 실패/미설정으로 폴백(초기 결과 반환)했는지 여부.
    pub fallback: bool,
    /// 종료/폴백 사유.
    pub reason: String,
}

/// 능동 검색 에이전트. 상한(재작성·LLM 호출)은 구조체 상태로, 나머지 튜닝은
/// `DeepSearchParams`로 주입받는다.
pub struct SearchAgent {
    max_rewrites: usize,
    max_llm_calls: usize,
}

impl Default for SearchAgent {
    fn default() -> Self {
        Self {
            max_rewrites: MAX_REWRITE_ROUNDS,
            max_llm_calls: MAX_AGENT_LLM_CALLS,
        }
    }
}

impl SearchAgent {
    pub fn new() -> Self {
        Self::default()
    }

    /// deep search 파이프라인을 실행한다. **에러를 반환하지 않는다** — 어떤 실패
    /// 모드에서도 (가능한) 결과와 메타데이터를 담은 `DeepSearchOutcome`을 돌려준다.
    ///
    /// 파이프라인:
    /// 1. 초기 hybrid 검색 (LLM 무의존).
    /// 2. LLM 가용 시: 충분성 평가 → [부족] 쿼리 재작성 재검색 루프
    ///    (상한: 재작성 라운드 / LLM 호출 / 시간 / 동일 쿼리 금지).
    /// 3. 폴백이 아니면 그래프 이웃 확장 (유래 표시).
    /// 4. 합성: 중복 제거 + 점수 재정렬 + 상위 N 절사.
    pub async fn deep_search(
        &self,
        backend: &dyn SearchBackend,
        llm: Option<&dyn LlmProvider>,
        query: &str,
        workspaces: &[String],
        params: DeepSearchParams,
    ) -> DeepSearchOutcome {
        let start = Instant::now();
        let mut queries: Vec<String> = vec![query.to_string()];
        let mut tried: HashSet<String> = HashSet::new();
        tried.insert(normalize_query(query));
        let mut accumulated: Vec<SearchResult> = Vec::new();
        let mut rounds = 0usize;

        // 1. 초기 검색 — 실패하면 반환할 것이 없어 빈 결과 + 폴백으로 종료(500 방지).
        match backend.run_search(query, workspaces).await {
            Ok(mut r) => {
                rounds += 1;
                accumulated.append(&mut r);
            }
            Err(e) => {
                return DeepSearchOutcome {
                    results: Vec::new(),
                    rounds: 0,
                    queries,
                    graph_expanded: false,
                    expansion_count: 0,
                    fallback: true,
                    reason: format!("초기 검색 실패로 빈 결과 반환: {e}"),
                };
            }
        }

        // 2. LLM 미설정이면 재작성 불가 — 초기 결과만 폴백 반환(확장도 생략: '그대로').
        let llm = match llm {
            Some(l) => l,
            None => {
                let results = synthesize(accumulated, params.max_results);
                return DeepSearchOutcome {
                    results,
                    rounds,
                    queries,
                    graph_expanded: false,
                    expansion_count: 0,
                    fallback: true,
                    reason: "LLM 미설정 — 초기 검색 결과 반환".to_string(),
                };
            }
        };

        // 재작성 루프. 상한을 모두 강제하고, LLM 실패는 폴백으로 흡수한다.
        let mut llm_calls = 0usize;
        let mut rewrites = 0usize;
        let mut fallback = false;
        let reason;

        loop {
            if rewrites >= self.max_rewrites {
                reason = "재작성 상한 도달".to_string();
                break;
            }
            if llm_calls >= self.max_llm_calls {
                reason = "LLM 호출 상한 도달".to_string();
                break;
            }
            if start.elapsed() >= params.time_limit {
                reason = "시간 상한 도달 — 부분 결과 반환".to_string();
                break;
            }

            // 충분성 평가 (LLM). 실패 시 폴백하고 루프 종료.
            let sufficiency = match self.evaluate_sufficiency(llm, query, &accumulated).await {
                Ok(s) => s,
                Err(e) => {
                    fallback = true;
                    reason = format!("충분성 평가 실패로 폴백: {e}");
                    break;
                }
            };
            llm_calls += 1;

            if sufficiency == Sufficiency::Sufficient {
                reason = "충분성 평가 통과 — 탐색 종료".to_string();
                break;
            }

            // 재작성 전에 호출 예산을 다시 확인 (평가로 예산이 소진됐을 수 있음).
            if llm_calls >= self.max_llm_calls {
                reason = "LLM 호출 상한 도달".to_string();
                break;
            }

            // 쿼리 재작성 (LLM). 실패 시 폴백, 재작성 없음이면 정상 종료.
            let rewritten = match self.rewrite_query(llm, query, &queries, &accumulated).await {
                Ok(Some(q)) => q,
                Ok(None) => {
                    reason = "재작성할 더 나은 쿼리 없음 — 종료".to_string();
                    break;
                }
                Err(e) => {
                    fallback = true;
                    reason = format!("쿼리 재작성 실패로 폴백: {e}");
                    break;
                }
            };
            llm_calls += 1;
            rewrites += 1;

            // 동일 쿼리 재검색 금지 — 재작성 결과가 이전과 같으면 루프 종료.
            if tried.contains(&normalize_query(&rewritten)) {
                reason = "재작성 결과가 이전 쿼리와 동일 — 루프 종료".to_string();
                break;
            }
            tried.insert(normalize_query(&rewritten));
            queries.push(rewritten.clone());

            // 재검색 — 실패 시 폴백하고 지금까지의 결과로 진행.
            match backend.run_search(&rewritten, workspaces).await {
                Ok(mut r) => {
                    rounds += 1;
                    accumulated.append(&mut r);
                }
                Err(e) => {
                    fallback = true;
                    reason = format!("재검색 실패로 폴백: {e}");
                    break;
                }
            }
        }

        // 3. 그래프 확장 — 폴백이 아닐 때만 (폴백은 '초기 결과 그대로' 시맨틱 보존).
        let mut graph_expanded = false;
        if !fallback && params.expansion_depth > 0 && !accumulated.is_empty() {
            let origins = top_origins(&accumulated, EXPANSION_ORIGIN_COUNT);
            if !origins.is_empty() {
                match backend.expand(&origins, params.expansion_depth).await {
                    Ok(expanded) => {
                        graph_expanded = !expanded.is_empty();
                        accumulated.extend(expanded);
                    }
                    Err(e) => {
                        // 확장 실패는 치명적이지 않다 — 이미 모은 결과로 계속(침묵하지 않음).
                        tracing::warn!("그래프 확장 실패(초기·재작성 결과로 계속): {}", e);
                    }
                }
            }
        }

        // 4. 합성 — 중복 제거 + 재정렬 + 상한 절사.
        let results = synthesize(accumulated, params.max_results);
        let expansion_count = results.iter().filter(|r| r.expanded_from.is_some()).count();

        DeepSearchOutcome {
            results,
            rounds,
            queries,
            graph_expanded,
            expansion_count,
            fallback,
            reason,
        }
    }

    /// 현재 결과가 질의에 충분한지 LLM으로 평가한다 (LLM 호출 1회).
    /// LLM 호출 실패는 `Err`로 전파해 상위 루프가 폴백하게 한다.
    pub async fn evaluate_sufficiency(
        &self,
        llm: &dyn LlmProvider,
        query: &str,
        results: &[SearchResult],
    ) -> Result<Sufficiency> {
        let prompt = build_sufficiency_prompt(query, results);
        let response = llm.complete(&prompt).await?;
        Ok(parse_sufficiency(&response))
    }

    /// 놓친 각도를 포착할 새 검색 쿼리를 LLM으로 제안한다 (LLM 호출 1회).
    ///
    /// 반환:
    /// - `Ok(Some(q))`: 새로운 쿼리 제안.
    /// - `Ok(None)`: LLM이 더 나은 쿼리를 내지 못함(정상 종료, 폴백 아님).
    /// - `Err`: LLM 호출 실패(상위 루프가 폴백).
    pub async fn rewrite_query(
        &self,
        llm: &dyn LlmProvider,
        original: &str,
        tried: &[String],
        results: &[SearchResult],
    ) -> Result<Option<String>> {
        let prompt = build_rewrite_prompt(original, tried, results);
        let response = llm.complete(&prompt).await?;
        Ok(parse_rewrite(&response))
    }
}

// ──────────────────────────────────────────────────────────────
// 순수 함수: 프롬프트 빌더 (중앙화 — 구조를 단위 테스트로 고정)
// ──────────────────────────────────────────────────────────────

/// 결과 요약 블록을 만든다 (프롬프트 비용 억제 — 요약만, 상한 개수).
fn result_summary_block(results: &[SearchResult]) -> String {
    const MAX_LISTED: usize = 10;
    if results.is_empty() {
        return "결과 없음".to_string();
    }
    results
        .iter()
        .take(MAX_LISTED)
        .map(|r| format!("- {}", r.summary))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 충분성 평가 프롬프트.
pub fn build_sufficiency_prompt(query: &str, results: &[SearchResult]) -> String {
    let result_block = result_summary_block(results);
    format!(
        r#"당신은 개인 지식 베이스의 검색 품질 평가자입니다.
사용자 질의에 대한 현재 검색 결과가 충분한지 보수적으로 판단합니다.

## 사용자 질의
"""
{query}
"""

## 현재 검색 결과 ({count}건)
{result_block}

## 판단 기준
- 결과가 질의의 핵심을 충분히 다루면 "충분"(sufficient=true).
- 명백히 누락된 각도가 있거나 결과가 비어 있으면 "부족"(sufficient=false).
- 애매하면 충분(true)으로 판단하세요 — 과잉 탐색보다 조기 종료가 낫습니다.

## 출력 형식 (JSON만, 다른 텍스트 없이)
{{"sufficient": true | false, "reason": "판단 근거 한 문장"}}"#,
        query = query,
        count = results.len(),
        result_block = result_block,
    )
}

/// 쿼리 재작성 프롬프트.
pub fn build_rewrite_prompt(original: &str, tried: &[String], results: &[SearchResult]) -> String {
    let tried_block = if tried.is_empty() {
        "없음".to_string()
    } else {
        tried
            .iter()
            .map(|q| format!("- {}", q))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let result_block = result_summary_block(results);
    format!(
        r#"당신은 개인 지식 베이스의 검색 쿼리 재작성기입니다.
현재 결과가 부족하여, 놓친 각도를 포착할 새로운 검색 쿼리를 제안합니다.

## 원 질의
"""
{original}
"""

## 이미 시도한 쿼리 (의미가 겹치지 않게 — 반복 금지)
{tried_block}

## 현재까지의 결과 ({count}건)
{result_block}

## 규칙
- 이미 시도한 쿼리와 의미가 겹치지 않는, 새로운 각도의 쿼리 1개를 제안하세요.
- 원 질의의 의미를 벗어나지 마세요 (무관한 주제로의 확장 금지).
- 동의어·상위어·관련 엔티티(사람/회사/기술)·인접 주제를 활용하세요.
- 더 나은 쿼리가 없으면 query를 빈 문자열로 두세요.

## 출력 형식 (JSON만, 다른 텍스트 없이)
{{"query": "새로운 검색 쿼리 또는 빈 문자열"}}"#,
        original = original,
        tried_block = tried_block,
        count = results.len(),
        result_block = result_block,
    )
}

// ──────────────────────────────────────────────────────────────
// 순수 함수: 응답 파서
// ──────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct RawSufficiency {
    #[serde(default)]
    sufficient: Option<bool>,
}

/// 충분성 평가 응답을 파싱한다 (순수 함수).
///
/// **보수적 기본값**: `sufficient=false`가 명시된 경우에만 부족으로 본다. true·누락·
/// 파싱 실패는 모두 "충분"으로 흡수한다 — 과잉 탐색보다 조기 종료를 선호하고,
/// LLM이 형식을 흔들어도 무한 재작성으로 빠지지 않게 하는 안전한 기본값이다.
pub fn parse_sufficiency(response: &str) -> Sufficiency {
    let json_str = extract_json(response);
    match serde_json::from_str::<RawSufficiency>(json_str) {
        Ok(raw) => match raw.sufficient {
            Some(false) => Sufficiency::Insufficient,
            _ => Sufficiency::Sufficient,
        },
        Err(_) => Sufficiency::Sufficient,
    }
}

#[derive(Debug, Deserialize)]
struct RawRewrite {
    #[serde(default)]
    query: String,
}

/// 재작성 응답을 파싱한다 (순수 함수).
///
/// 비어 있지 않은 `query`만 채택한다. JSON 파싱 실패나 빈 쿼리는 `None`으로 흡수해
/// 상위가 루프를 정상 종료하게 한다(쓰레기 쿼리로 재검색하지 않음).
pub fn parse_rewrite(response: &str) -> Option<String> {
    let json_str = extract_json(response);
    match serde_json::from_str::<RawRewrite>(json_str) {
        Ok(raw) => {
            let q = raw.query.trim();
            if q.is_empty() {
                None
            } else {
                Some(q.to_string())
            }
        }
        Err(_) => None,
    }
}

// ──────────────────────────────────────────────────────────────
// 순수 함수: 합성 (중복 제거 · 재정렬 · 절사) + 확장 점수
// ──────────────────────────────────────────────────────────────

/// 쿼리 정규화 — 동일 쿼리 재검색 판정용 (공백 트림 + 소문자).
fn normalize_query(q: &str) -> String {
    q.trim().to_lowercase()
}

/// 그래프 확장으로 끌어온 이웃의 점수를 계산한다 (순수 함수).
///
/// `origin_score × edge_weight × DISCOUNT`. 엣지 가중치(≤1)와 감쇠 계수로 인해
/// 확장 문서는 그 출처보다 낮은 점수를 받아, 직접 매칭 결과를 밀어내지 않고
/// 보완만 한다. 결과는 [0,1]로 클램프된다.
pub fn expansion_score(origin_score: f32, edge_weight: f32) -> f32 {
    (origin_score * edge_weight.clamp(0.0, 1.0) * EXPANSION_SCORE_DISCOUNT).clamp(0.0, 1.0)
}

/// 그래프 확장의 출처가 될 상위 결과를 고른다 (id 중복 제거 후 점수 상위 k).
///
/// 동점은 id로 tie-break해 결정적이다. 확장은 이 출처들의 이웃만 따라가므로
/// fan-out이 상한된다.
fn top_origins(results: &[SearchResult], k: usize) -> Vec<ExpandOrigin> {
    let mut best: HashMap<Uuid, (f32, String)> = HashMap::new();
    for r in results {
        let entry = best
            .entry(r.id)
            .or_insert((r.relevance_score, r.workspace.clone()));
        if r.relevance_score > entry.0 {
            *entry = (r.relevance_score, r.workspace.clone());
        }
    }

    let mut origins: Vec<ExpandOrigin> = best
        .into_iter()
        .map(|(id, (score, workspace))| ExpandOrigin {
            id,
            workspace,
            score,
        })
        .collect();
    origins.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.id.cmp(&b.id))
    });
    origins.truncate(k);
    origins
}

/// 여러 라운드·확장 결과를 합성한다: 문서 id 기준 중복 제거 → 점수 내림차순
/// 재정렬 → 상위 `max_results` 절사 (순수 함수).
///
/// 중복 제거는 **더 높은 점수**를 유지하고, 동점이면 직접 검색 결과(expanded_from
/// None)를 그래프 확장 결과보다 우선한다 — 같은 문서가 직접 매칭과 확장 양쪽으로
/// 들어와도 직접 유래·최고 점수가 남는다.
pub fn synthesize(results: Vec<SearchResult>, max_results: usize) -> Vec<SearchResult> {
    let mut deduped = dedup_by_best(results);
    deduped.sort_by(|a, b| {
        b.relevance_score
            .partial_cmp(&a.relevance_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    deduped.truncate(max_results);
    deduped
}

/// 문서 id별 최적(최고 점수, 동점 시 직접 결과 우선) 항목만 남긴다. 첫 등장 순서를
/// 보존해 이후 안정 정렬의 tie-break가 결정적이게 한다.
fn dedup_by_best(results: Vec<SearchResult>) -> Vec<SearchResult> {
    let mut best: HashMap<Uuid, SearchResult> = HashMap::new();
    let mut order: Vec<Uuid> = Vec::new();

    for r in results {
        match best.get_mut(&r.id) {
            Some(existing) => {
                let higher = r.relevance_score > existing.relevance_score;
                let tie_prefer_direct = (r.relevance_score - existing.relevance_score).abs()
                    < f32::EPSILON
                    && existing.expanded_from.is_some()
                    && r.expanded_from.is_none();
                if higher || tie_prefer_direct {
                    *existing = r;
                }
            }
            None => {
                order.push(r.id);
                best.insert(r.id, r);
            }
        }
    }

    order
        .into_iter()
        .filter_map(|id| best.remove(&id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ProviderType;
    use crate::models::ParsedContent;
    use anyhow::anyhow;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    // ──── 테스트 헬퍼 ────

    fn direct(id: Uuid, score: f32) -> SearchResult {
        SearchResult {
            id,
            summary: format!("직접 결과 {score}"),
            relevance_score: score,
            workspace: "default".to_string(),
            matched_facts: vec![],
            created_at: None,
            expanded_from: None,
        }
    }

    fn expanded(id: Uuid, origin: Uuid, score: f32) -> SearchResult {
        SearchResult {
            expanded_from: Some(origin),
            summary: format!("확장 결과 {score}"),
            ..direct(id, score)
        }
    }

    fn params() -> DeepSearchParams {
        DeepSearchParams {
            expansion_depth: 1,
            time_limit: Duration::from_secs(3600),
            max_results: DEFAULT_DEEP_SEARCH_MAX_RESULTS,
        }
    }

    /// 순차 응답을 반환하는 mock LLM (ingest_agent 테스트와 동일 패턴).
    struct MockLlm {
        responses: Mutex<VecDeque<Result<String>>>,
        calls: Mutex<usize>,
    }

    impl MockLlm {
        fn new(responses: Vec<Result<String>>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().collect()),
                calls: Mutex::new(0),
            }
        }
        fn call_count(&self) -> usize {
            *self.calls.lock().unwrap()
        }
    }

    #[async_trait]
    impl LlmProvider for MockLlm {
        fn provider_type(&self) -> ProviderType {
            ProviderType::Gemini
        }
        async fn parse(&self, _content: &str) -> Result<ParsedContent> {
            unimplemented!("mock은 complete만 사용")
        }
        async fn complete(&self, _prompt: &str) -> Result<String> {
            *self.calls.lock().unwrap() += 1;
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(anyhow!("응답 소진")))
        }
        async fn validate_api_key(&self) -> Result<bool> {
            Ok(true)
        }
    }

    /// 검색/확장을 흉내내는 mock backend. 큐가 비면 `when_empty`(검색)·빈 벡터(확장)를
    /// 반환한다. 에러 케이스는 큐에 `Err(())`로 넣는다(anyhow는 non-Clone이라 유닛 에러 사용).
    struct MockBackend {
        search_queue: Mutex<VecDeque<std::result::Result<Vec<SearchResult>, ()>>>,
        when_empty: Vec<SearchResult>,
        expand_queue: Mutex<VecDeque<std::result::Result<Vec<SearchResult>, ()>>>,
        search_calls: Mutex<usize>,
        expand_calls: Mutex<usize>,
    }

    impl MockBackend {
        fn new(
            search: Vec<std::result::Result<Vec<SearchResult>, ()>>,
            when_empty: Vec<SearchResult>,
            expand: Vec<std::result::Result<Vec<SearchResult>, ()>>,
        ) -> Self {
            Self {
                search_queue: Mutex::new(search.into_iter().collect()),
                when_empty,
                expand_queue: Mutex::new(expand.into_iter().collect()),
                search_calls: Mutex::new(0),
                expand_calls: Mutex::new(0),
            }
        }
        fn search_calls(&self) -> usize {
            *self.search_calls.lock().unwrap()
        }
        fn expand_calls(&self) -> usize {
            *self.expand_calls.lock().unwrap()
        }
    }

    #[async_trait]
    impl SearchBackend for MockBackend {
        async fn run_search(
            &self,
            _query: &str,
            _workspaces: &[String],
        ) -> Result<Vec<SearchResult>> {
            *self.search_calls.lock().unwrap() += 1;
            match self.search_queue.lock().unwrap().pop_front() {
                Some(Ok(v)) => Ok(v),
                Some(Err(())) => Err(anyhow!("mock 검색 에러")),
                None => Ok(self.when_empty.clone()),
            }
        }
        async fn expand(
            &self,
            _origins: &[ExpandOrigin],
            _depth: usize,
        ) -> Result<Vec<SearchResult>> {
            *self.expand_calls.lock().unwrap() += 1;
            match self.expand_queue.lock().unwrap().pop_front() {
                Some(Ok(v)) => Ok(v),
                Some(Err(())) => Err(anyhow!("mock 확장 에러")),
                None => Ok(vec![]),
            }
        }
    }

    // ──── 순수 함수: 파서 ────

    #[test]
    fn test_parse_sufficiency_true() {
        assert_eq!(
            parse_sufficiency(r#"{"sufficient":true,"reason":"충분"}"#),
            Sufficiency::Sufficient
        );
    }

    #[test]
    fn test_parse_sufficiency_false() {
        assert_eq!(
            parse_sufficiency(r#"{"sufficient":false,"reason":"부족"}"#),
            Sufficiency::Insufficient
        );
    }

    #[test]
    fn test_parse_sufficiency_missing_defaults_sufficient() {
        // 보수적 기본값: 필드 누락 → 충분(조기 종료).
        assert_eq!(parse_sufficiency(r#"{"reason":"x"}"#), Sufficiency::Sufficient);
    }

    #[test]
    fn test_parse_sufficiency_garbage_defaults_sufficient() {
        // 파싱 실패 → 충분(무한 재작성 방지).
        assert_eq!(parse_sufficiency("not json"), Sufficiency::Sufficient);
    }

    #[test]
    fn test_parse_sufficiency_code_fence() {
        let resp = "```json\n{\"sufficient\":false}\n```";
        assert_eq!(parse_sufficiency(resp), Sufficiency::Insufficient);
    }

    #[test]
    fn test_parse_rewrite_valid() {
        assert_eq!(
            parse_rewrite(r#"{"query":"전입신고 절차"}"#),
            Some("전입신고 절차".to_string())
        );
    }

    #[test]
    fn test_parse_rewrite_empty_is_none() {
        assert_eq!(parse_rewrite(r#"{"query":""}"#), None);
        assert_eq!(parse_rewrite(r#"{"query":"   "}"#), None);
    }

    #[test]
    fn test_parse_rewrite_garbage_is_none() {
        assert_eq!(parse_rewrite("가나다"), None);
    }

    #[test]
    fn test_parse_rewrite_code_fence() {
        assert_eq!(
            parse_rewrite("```json\n{\"query\":\"계약서\"}\n```"),
            Some("계약서".to_string())
        );
    }

    // ──── 순수 함수: 프롬프트 빌더 ────

    #[test]
    fn test_sufficiency_prompt_includes_query_and_results() {
        let results = vec![direct(Uuid::new_v4(), 0.8)];
        let prompt = build_sufficiency_prompt("이사 관련 전부", &results);
        assert!(prompt.contains("이사 관련 전부"), "질의 누락");
        assert!(prompt.contains("직접 결과 0.8"), "결과 요약 누락");
        assert!(prompt.contains("sufficient"), "출력 형식 키워드 누락");
    }

    #[test]
    fn test_sufficiency_prompt_empty_results() {
        let prompt = build_sufficiency_prompt("q", &[]);
        assert!(prompt.contains("결과 없음"));
    }

    #[test]
    fn test_rewrite_prompt_includes_original_and_tried() {
        let results = vec![direct(Uuid::new_v4(), 0.7)];
        let tried = vec!["이사".to_string(), "계약".to_string()];
        let prompt = build_rewrite_prompt("이사 관련 전부", &tried, &results);
        assert!(prompt.contains("이사 관련 전부"), "원 질의 누락");
        assert!(prompt.contains("- 이사"), "시도 쿼리 누락");
        assert!(prompt.contains("- 계약"), "시도 쿼리 누락");
        assert!(prompt.contains("query"), "출력 형식 키워드 누락");
    }

    // ──── 순수 함수: 확장 점수 ────

    #[test]
    fn test_expansion_score_discounts_below_origin() {
        // 확장 문서는 출처보다 낮은 점수를 받아야 한다(직접 매칭 우선).
        let origin = 0.8;
        let s = expansion_score(origin, 1.0);
        assert!(s < origin, "확장 점수 {s}는 출처 {origin}보다 낮아야 한다");
    }

    #[test]
    fn test_expansion_score_edge_weight_scales() {
        let strong = expansion_score(0.8, 1.0);
        let weak = expansion_score(0.8, 0.3);
        assert!(strong > weak, "엣지 가중치가 클수록 확장 점수가 커야 한다");
    }

    #[test]
    fn test_expansion_score_clamped() {
        assert!((expansion_score(2.0, 2.0) - 1.0).abs() < f32::EPSILON, "1.0으로 클램프");
        assert!(expansion_score(0.5, -1.0) >= 0.0, "음수 방지");
    }

    // ──── 순수 함수: 합성 (중복 제거·재정렬·절사) ────

    #[test]
    fn test_synthesize_sorts_by_score_desc() {
        let results = vec![direct(Uuid::new_v4(), 0.3), direct(Uuid::new_v4(), 0.9), direct(Uuid::new_v4(), 0.6)];
        let out = synthesize(results, 10);
        assert_eq!(out.len(), 3);
        assert!(out[0].relevance_score >= out[1].relevance_score);
        assert!(out[1].relevance_score >= out[2].relevance_score);
        assert!((out[0].relevance_score - 0.9).abs() < f32::EPSILON);
    }

    #[test]
    fn test_synthesize_dedups_keeping_higher_score() {
        // 같은 문서가 여러 라운드에서 다른 점수로 들어오면 최고 점수만 남는다.
        let id = Uuid::new_v4();
        let results = vec![direct(id, 0.5), direct(id, 0.85), direct(Uuid::new_v4(), 0.4)];
        let out = synthesize(results, 10);
        assert_eq!(out.len(), 2, "중복 문서는 하나로 합쳐져야 한다");
        let kept = out.iter().find(|r| r.id == id).unwrap();
        assert!((kept.relevance_score - 0.85).abs() < f32::EPSILON, "최고 점수 유지");
    }

    #[test]
    fn test_synthesize_dedup_prefers_direct_on_tie() {
        // 동점이면 직접 결과(expanded_from None)가 확장 결과를 이긴다.
        let id = Uuid::new_v4();
        let origin = Uuid::new_v4();
        let results = vec![expanded(id, origin, 0.7), direct(id, 0.7)];
        let out = synthesize(results, 10);
        assert_eq!(out.len(), 1);
        assert!(out[0].expanded_from.is_none(), "동점 시 직접 유래가 남아야 한다");
    }

    #[test]
    fn test_synthesize_truncates_to_max() {
        let results: Vec<_> = (0..30).map(|i| direct(Uuid::new_v4(), i as f32 / 30.0)).collect();
        let out = synthesize(results, 5);
        assert_eq!(out.len(), 5, "상한으로 절사되어야 한다");
    }

    #[test]
    fn test_synthesize_no_duplicate_ids() {
        // PRD 인수 조건: 결과에 중복 문서가 없어야 한다.
        let id = Uuid::new_v4();
        let results = vec![direct(id, 0.5), direct(id, 0.6), direct(id, 0.9)];
        let out = synthesize(results, 10);
        let unique: HashSet<Uuid> = out.iter().map(|r| r.id).collect();
        assert_eq!(unique.len(), out.len(), "중복 id가 없어야 한다");
    }

    #[test]
    fn test_top_origins_dedup_and_topk() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let c = Uuid::new_v4();
        let results = vec![direct(a, 0.9), direct(a, 0.5), direct(b, 0.7), direct(c, 0.3)];
        let origins = top_origins(&results, 2);
        assert_eq!(origins.len(), 2, "상위 2개만");
        assert_eq!(origins[0].id, a, "최고 점수가 첫 번째");
        assert!((origins[0].score - 0.9).abs() < f32::EPSILON, "중복 시 최고 점수 유지");
        assert_eq!(origins[1].id, b);
    }

    // ──── mock LLM: 충분성 평가 / 재작성 메서드 ────

    #[tokio::test]
    async fn test_evaluate_sufficiency_success() {
        let llm = MockLlm::new(vec![Ok(r#"{"sufficient":false}"#.to_string())]);
        let agent = SearchAgent::new();
        let s = agent.evaluate_sufficiency(&llm, "q", &[]).await.unwrap();
        assert_eq!(s, Sufficiency::Insufficient);
    }

    #[tokio::test]
    async fn test_evaluate_sufficiency_llm_error_propagates() {
        let llm = MockLlm::new(vec![Err(anyhow!("장애"))]);
        let agent = SearchAgent::new();
        assert!(agent.evaluate_sufficiency(&llm, "q", &[]).await.is_err());
    }

    #[tokio::test]
    async fn test_rewrite_query_success() {
        let llm = MockLlm::new(vec![Ok(r#"{"query":"전입신고"}"#.to_string())]);
        let agent = SearchAgent::new();
        let q = agent.rewrite_query(&llm, "이사", &["이사".to_string()], &[]).await.unwrap();
        assert_eq!(q, Some("전입신고".to_string()));
    }

    #[tokio::test]
    async fn test_rewrite_query_empty_is_none() {
        let llm = MockLlm::new(vec![Ok(r#"{"query":""}"#.to_string())]);
        let agent = SearchAgent::new();
        let q = agent.rewrite_query(&llm, "이사", &[], &[]).await.unwrap();
        assert_eq!(q, None);
    }

    #[tokio::test]
    async fn test_rewrite_query_llm_error_propagates() {
        let llm = MockLlm::new(vec![Err(anyhow!("장애"))]);
        let agent = SearchAgent::new();
        assert!(agent.rewrite_query(&llm, "이사", &[], &[]).await.is_err());
    }

    // ──── 파이프라인: 충분 분기 (재작성 없음) ────

    #[tokio::test]
    async fn test_pipeline_sufficient_no_rewrite() {
        // 초기 검색 → 충분 → 재작성 없이 종료. 확장은 실행됨.
        let init = vec![direct(Uuid::new_v4(), 0.8)];
        let backend = MockBackend::new(vec![Ok(init)], vec![], vec![Ok(vec![])]);
        let llm = MockLlm::new(vec![Ok(r#"{"sufficient":true}"#.to_string())]);
        let agent = SearchAgent::new();

        let out = agent
            .deep_search(&backend, Some(&llm), "이사", &["default".to_string()], params())
            .await;

        assert_eq!(out.rounds, 1, "재작성 없이 1라운드");
        assert_eq!(out.queries, vec!["이사".to_string()]);
        assert!(!out.fallback);
        assert_eq!(backend.search_calls(), 1);
        assert_eq!(llm.call_count(), 1, "충분성 평가 1회, 재작성 0회");
        assert_eq!(backend.expand_calls(), 1, "폴백 아니면 확장 시도");
    }

    // ──── 파이프라인: 부족 → 재작성 분기 ────

    #[tokio::test]
    async fn test_pipeline_insufficient_then_rewrite_then_sufficient() {
        let init = vec![direct(Uuid::new_v4(), 0.6)];
        let round2 = vec![direct(Uuid::new_v4(), 0.7)];
        let backend = MockBackend::new(vec![Ok(init), Ok(round2)], vec![], vec![Ok(vec![])]);
        // eval→부족, rewrite→새 쿼리, eval→충분
        let llm = MockLlm::new(vec![
            Ok(r#"{"sufficient":false}"#.to_string()),
            Ok(r#"{"query":"전입신고"}"#.to_string()),
            Ok(r#"{"sufficient":true}"#.to_string()),
        ]);
        let agent = SearchAgent::new();

        let out = agent
            .deep_search(&backend, Some(&llm), "이사", &["default".to_string()], params())
            .await;

        assert_eq!(out.rounds, 2, "초기 + 재작성 재검색 = 2라운드");
        assert_eq!(out.queries, vec!["이사".to_string(), "전입신고".to_string()]);
        assert!(!out.fallback);
        assert_eq!(backend.search_calls(), 2);
        // 결과에 양 라운드 문서가 모두 포함(중복 없음).
        assert_eq!(out.results.len(), 2);
    }

    // ──── 파이프라인: 재작성 상한 (무한 루프 방지) ────

    #[tokio::test]
    async fn test_pipeline_rewrite_loop_terminates_at_cap() {
        // 항상 부족 + 매번 새 유니크 쿼리를 줘도 상한에서 반드시 멈춘다.
        let mut llm_responses = Vec::new();
        for i in 0..10 {
            llm_responses.push(Ok(r#"{"sufficient":false}"#.to_string()));
            llm_responses.push(Ok(format!(r#"{{"query":"쿼리{i}"}}"#)));
        }
        let backend = MockBackend::new(vec![], vec![direct(Uuid::new_v4(), 0.5)], vec![Ok(vec![])]);
        let llm = MockLlm::new(llm_responses);
        let agent = SearchAgent::new();

        let out = agent
            .deep_search(&backend, Some(&llm), "q", &["default".to_string()], params())
            .await;

        // 상한 강제: 재작성 ≤ 3, LLM 호출 ≤ 5, 라운드 ≤ 1+3.
        assert!(out.rounds <= 1 + MAX_REWRITE_ROUNDS, "라운드가 상한을 넘음: {}", out.rounds);
        assert!(llm.call_count() <= MAX_AGENT_LLM_CALLS, "LLM 호출이 상한을 넘음: {}", llm.call_count());
        assert!(!out.fallback, "정상 상한 도달은 폴백이 아니다");
    }

    #[tokio::test]
    async fn test_pipeline_respects_llm_call_cap() {
        // LLM 호출 상한(5)을 절대 넘지 않는다.
        let mut llm_responses = Vec::new();
        for i in 0..10 {
            llm_responses.push(Ok(r#"{"sufficient":false}"#.to_string()));
            llm_responses.push(Ok(format!(r#"{{"query":"q{i}"}}"#)));
        }
        let backend = MockBackend::new(vec![], vec![direct(Uuid::new_v4(), 0.5)], vec![Ok(vec![])]);
        let llm = MockLlm::new(llm_responses);
        let agent = SearchAgent::new();
        agent.deep_search(&backend, Some(&llm), "q", &["default".to_string()], params()).await;
        assert!(llm.call_count() <= MAX_AGENT_LLM_CALLS);
    }

    // ──── 파이프라인: 동일 쿼리 재작성 → 종료 ────

    #[tokio::test]
    async fn test_pipeline_duplicate_rewrite_terminates() {
        // 재작성이 원 쿼리와 동일하면 재검색하지 않고 루프 종료.
        let backend = MockBackend::new(
            vec![Ok(vec![direct(Uuid::new_v4(), 0.5)])],
            vec![],
            vec![Ok(vec![])],
        );
        let llm = MockLlm::new(vec![
            Ok(r#"{"sufficient":false}"#.to_string()),
            Ok(r#"{"query":"이사"}"#.to_string()), // 원 쿼리와 동일
        ]);
        let agent = SearchAgent::new();

        let out = agent
            .deep_search(&backend, Some(&llm), "이사", &["default".to_string()], params())
            .await;

        assert_eq!(out.rounds, 1, "동일 쿼리는 재검색하지 않는다");
        assert_eq!(backend.search_calls(), 1);
        assert!(out.reason.contains("동일"), "종료 사유가 명시되어야 한다: {}", out.reason);
    }

    // ──── 파이프라인: 그래프 확장 (유래 표시) ────

    #[tokio::test]
    async fn test_pipeline_graph_expansion_tags_provenance() {
        let origin_id = Uuid::new_v4();
        let neighbor_id = Uuid::new_v4();
        let init = vec![direct(origin_id, 0.8)];
        let expansion = vec![expanded(neighbor_id, origin_id, 0.5)];
        let backend = MockBackend::new(vec![Ok(init)], vec![], vec![Ok(expansion)]);
        let llm = MockLlm::new(vec![Ok(r#"{"sufficient":true}"#.to_string())]);
        let agent = SearchAgent::new();

        let out = agent
            .deep_search(&backend, Some(&llm), "이사", &["default".to_string()], params())
            .await;

        assert!(out.graph_expanded, "확장이 이웃을 반환했으면 true");
        assert_eq!(out.expansion_count, 1);
        assert_eq!(out.results.len(), 2, "직접 결과 + 확장 이웃");
        let neighbor = out.results.iter().find(|r| r.id == neighbor_id).unwrap();
        assert_eq!(neighbor.expanded_from, Some(origin_id), "확장 유래가 표시되어야 한다");
    }

    #[tokio::test]
    async fn test_pipeline_expansion_skipped_when_depth_zero() {
        let init = vec![direct(Uuid::new_v4(), 0.8)];
        let backend = MockBackend::new(vec![Ok(init)], vec![], vec![Ok(vec![])]);
        let llm = MockLlm::new(vec![Ok(r#"{"sufficient":true}"#.to_string())]);
        let agent = SearchAgent::new();

        let mut p = params();
        p.expansion_depth = 0;
        let out = agent.deep_search(&backend, Some(&llm), "q", &["default".to_string()], p).await;

        assert_eq!(backend.expand_calls(), 0, "depth=0이면 확장하지 않는다");
        assert!(!out.graph_expanded);
    }

    // ──── 파이프라인: 폴백 (LLM 실패 / 미설정) ────

    #[tokio::test]
    async fn test_pipeline_llm_failure_falls_back_to_initial() {
        // 첫 충분성 평가에서 LLM 장애 → 초기 결과 + 폴백, 확장 없음.
        let init = vec![direct(Uuid::new_v4(), 0.7), direct(Uuid::new_v4(), 0.6)];
        let backend = MockBackend::new(vec![Ok(init)], vec![], vec![Ok(vec![])]);
        let llm = MockLlm::new(vec![Err(anyhow!("LLM 장애"))]);
        let agent = SearchAgent::new();

        let out = agent
            .deep_search(&backend, Some(&llm), "이사", &["default".to_string()], params())
            .await;

        assert!(out.fallback, "LLM 실패 시 폴백");
        assert_eq!(out.results.len(), 2, "초기 검색 결과가 그대로 반환");
        assert_eq!(out.rounds, 1);
        assert_eq!(backend.expand_calls(), 0, "폴백에서는 확장하지 않는다");
        assert!(out.reason.contains("폴백"));
    }

    #[tokio::test]
    async fn test_pipeline_no_llm_falls_back_to_initial() {
        // LLM 미설정 → 초기 결과 + 폴백, 확장·재작성 없음.
        let init = vec![direct(Uuid::new_v4(), 0.7)];
        let backend = MockBackend::new(vec![Ok(init)], vec![], vec![Ok(vec![])]);
        let agent = SearchAgent::new();

        let out = agent
            .deep_search(&backend, None, "이사", &["default".to_string()], params())
            .await;

        assert!(out.fallback);
        assert_eq!(out.results.len(), 1);
        assert_eq!(backend.expand_calls(), 0);
        assert!(out.reason.contains("LLM 미설정"));
    }

    #[tokio::test]
    async fn test_pipeline_initial_search_failure_returns_empty_fallback() {
        // 초기 검색 자체가 실패 → 빈 결과 + 폴백(에러 아님).
        let backend = MockBackend::new(vec![Err(())], vec![], vec![Ok(vec![])]);
        let llm = MockLlm::new(vec![Ok(r#"{"sufficient":true}"#.to_string())]);
        let agent = SearchAgent::new();

        let out = agent
            .deep_search(&backend, Some(&llm), "q", &["default".to_string()], params())
            .await;

        assert!(out.fallback);
        assert!(out.results.is_empty());
        assert_eq!(out.rounds, 0);
        assert_eq!(llm.call_count(), 0, "초기 검색 실패 시 LLM을 호출하지 않는다");
    }

    // ──── 파이프라인: 시간 상한 ────

    #[tokio::test]
    async fn test_pipeline_time_limit_zero_skips_rewrite() {
        // 시간 상한 0 → 초기 검색 후 재작성 루프 진입 전 종료(부분 결과). 확장은 실행.
        let init = vec![direct(Uuid::new_v4(), 0.8)];
        let backend = MockBackend::new(vec![Ok(init)], vec![], vec![Ok(vec![])]);
        let llm = MockLlm::new(vec![]);
        let agent = SearchAgent::new();

        let mut p = params();
        p.time_limit = Duration::ZERO;
        let out = agent.deep_search(&backend, Some(&llm), "q", &["default".to_string()], p).await;

        assert_eq!(out.rounds, 1, "초기 결과만");
        assert_eq!(llm.call_count(), 0, "시간 상한으로 LLM 미호출");
        assert!(!out.fallback, "시간 상한은 폴백이 아니다");
        assert!(out.reason.contains("시간 상한"));
    }

    // ──── 파이프라인: 부족 인지 (zero-result 재작성 시도, scenario 2) ────

    #[tokio::test]
    async fn test_pipeline_zero_result_tries_rewrite_then_returns_tried_queries() {
        // 초기 0건 → 부족 판단 → 재작성 → 여전히 0건 → 충분 → 빈 결과 + 시도 쿼리 목록.
        let backend = MockBackend::new(vec![Ok(vec![]), Ok(vec![])], vec![], vec![Ok(vec![])]);
        let llm = MockLlm::new(vec![
            Ok(r#"{"sufficient":false}"#.to_string()),
            Ok(r#"{"query":"양자컴퓨터 학습"}"#.to_string()),
            Ok(r#"{"sufficient":true}"#.to_string()),
        ]);
        let agent = SearchAgent::new();

        let out = agent
            .deep_search(&backend, Some(&llm), "양자컴퓨터 공부 기록", &["default".to_string()], params())
            .await;

        assert!(out.results.is_empty(), "관련 지식 없음 — 거짓 결과를 만들지 않는다");
        assert_eq!(out.queries.len(), 2, "시도한 쿼리들이 남아야 한다");
        assert!(out.queries.contains(&"양자컴퓨터 학습".to_string()));
        assert!(!out.fallback);
    }

    // ──── 파이프라인: agent가 더 넓은 결과 집합 반환 (인수 조건) ────

    #[tokio::test]
    async fn test_pipeline_agent_returns_broader_set_than_single_round() {
        // 재작성 확장 + 그래프 확장으로 단일 라운드보다 넓은 집합을 반환한다.
        let d1 = Uuid::new_v4();
        let d2 = Uuid::new_v4();
        let d3 = Uuid::new_v4();
        let neighbor = Uuid::new_v4();
        let init = vec![direct(d1, 0.8)];
        let round2 = vec![direct(d2, 0.7), direct(d3, 0.65)];
        let expansion = vec![expanded(neighbor, d1, 0.5)];
        let backend = MockBackend::new(vec![Ok(init), Ok(round2)], vec![], vec![Ok(expansion)]);
        let llm = MockLlm::new(vec![
            Ok(r#"{"sufficient":false}"#.to_string()),
            Ok(r#"{"query":"이사 계약"}"#.to_string()),
            Ok(r#"{"sufficient":true}"#.to_string()),
        ]);
        let agent = SearchAgent::new();

        let out = agent
            .deep_search(&backend, Some(&llm), "이사 관련 전부", &["default".to_string()], params())
            .await;

        // 단일 라운드(초기)는 1건이었지만, agent는 재작성 2건 + 확장 1건 = 4건으로 넓어진다.
        assert_eq!(out.results.len(), 4, "agent가 더 넓은 집합을 반환해야 한다: {:?}", out.results.len());
        assert!(out.graph_expanded);
        assert_eq!(out.rounds, 2);
    }
}
