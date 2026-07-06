//! Ingest Agent — 저장 전에 신규/업데이트/분할/중복을 판단하는 LLM 에이전트.
//!
//! 이 모듈은 **판단(decision)만** 담당한다. 실제 저장(분할 실행·엣지 생성·버전
//! 보관)은 `Indexer`가 기존 인덱싱 파이프라인을 실행기로 재사용한다.
//!
//! 설계 원칙:
//! - **정보 유실 0**: 어떤 실패 모드에서도 입력이 사라지는 경로가 없다. 판단이
//!   실패하면(파싱 불가/타임아웃) 호출 측이 raw 저장으로 폴백한다. 판단이 애매하면
//!   (환각 target, 무의미한 분할) 안전하게 New로 강등한다.
//! - **상한**: 분할 최대 개수, LLM 재시도 횟수 모두 상수 상한.
//! - **테스트 용이성**: 프롬프트 빌더·응답 파서를 순수 함수로 분리해 mock 없이
//!   검증하고, `decide`는 `LlmProvider` mock으로 각 분기·폴백을 고정한다.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use uuid::Uuid;

use crate::llm::{extract_json, LlmProvider};
use crate::models::RelationType;

/// 자동 생성 엣지의 초기 가중치. 단순 상수 — Phase 5에서 감쇠 재계산된다.
pub const DEFAULT_EDGE_WEIGHT: f32 = 0.5;

/// 1개 입력을 분할할 수 있는 최대 문서 수 (PRD: 1입력 최대 5문서).
pub const DEFAULT_MAX_SPLIT: usize = 5;

/// 판단 LLM 호출 최대 시도 횟수 (최초 1 + 재시도 1).
const MAX_DECISION_ATTEMPTS: usize = 2;

/// Ingest Agent가 결정하는 저장 전략.
#[derive(Debug, Clone, PartialEq)]
pub enum IngestStrategy {
    /// 기존과 무관한 새 정보 → 신규 문서로 저장
    New,
    /// 기존 문서와 같은 주제의 갱신 정보 → 그 문서를 업데이트(이전 버전 보관)
    Update { target: Uuid },
    /// 서로 독립적인 여러 주제가 섞임 → 주제별로 분할 저장
    Split { segments: Vec<String> },
    /// 기존 문서와 사실상 동일 → 중복 (저장 실행은 하지 않고 판단만 기록)
    Duplicate { of: Uuid },
}

impl IngestStrategy {
    /// API/로그 표기용 라벨.
    pub fn label(&self) -> &'static str {
        match self {
            IngestStrategy::New => "new",
            IngestStrategy::Update { .. } => "update",
            IngestStrategy::Split { .. } => "split",
            IngestStrategy::Duplicate { .. } => "duplicate",
        }
    }
}

/// 판단 결과 — 전략과 그 근거.
#[derive(Debug, Clone)]
pub struct AgentDecision {
    pub strategy: IngestStrategy,
    pub reason: String,
}

/// 판단에 참고할 기존 관련 문서 후보 (요약만 전달 — 프롬프트 비용 억제).
#[derive(Debug, Clone)]
pub struct CandidateDoc {
    pub id: Uuid,
    pub summary: String,
}

/// LLM이 반환하는 판단 JSON의 원시 형태.
#[derive(Debug, Deserialize)]
struct RawDecision {
    strategy: String,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    segments: Vec<String>,
    #[serde(default)]
    reason: String,
}

/// 저장 판단 에이전트.
pub struct IngestAgent {
    max_split: usize,
}

impl Default for IngestAgent {
    fn default() -> Self {
        Self {
            max_split: DEFAULT_MAX_SPLIT,
        }
    }
}

impl IngestAgent {
    pub fn new() -> Self {
        Self::default()
    }

    /// 분할 상한을 지정해 생성한다. 0이 들어오면 1로 보정한다(최소 상한 보장).
    pub fn with_max_split(max_split: usize) -> Self {
        Self {
            max_split: max_split.max(1),
        }
    }

    /// 입력과 후보 문서를 바탕으로 저장 전략을 판단한다.
    ///
    /// LLM 호출과 파싱을 최대 `MAX_DECISION_ATTEMPTS`회 시도한다. 모두 실패하면
    /// `Err`를 반환하며, 호출 측은 이를 받아 raw 저장으로 폴백해야 한다(정보 유실 0).
    pub async fn decide(
        &self,
        llm: &dyn LlmProvider,
        input: &str,
        candidates: &[CandidateDoc],
    ) -> Result<AgentDecision> {
        let prompt = build_ingest_decision_prompt(input, candidates, self.max_split);

        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..MAX_DECISION_ATTEMPTS {
            match llm.complete(&prompt).await {
                Ok(response) => match parse_ingest_decision(&response, candidates, self.max_split) {
                    Ok(decision) => return Ok(decision),
                    Err(e) => {
                        tracing::warn!(
                            "Ingest 판단 파싱 실패 (시도 {}/{}): {}",
                            attempt + 1,
                            MAX_DECISION_ATTEMPTS,
                            e
                        );
                        last_err = Some(e);
                    }
                },
                Err(e) => {
                    tracing::warn!(
                        "Ingest 판단 LLM 호출 실패 (시도 {}/{}): {}",
                        attempt + 1,
                        MAX_DECISION_ATTEMPTS,
                        e
                    );
                    last_err = Some(e);
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow!("Ingest 판단이 실패했습니다")))
    }

    /// 출발 문서와 대상 문서들 사이의 관계 타입을 판단한다 (LLM 호출 1회).
    ///
    /// **폴백 내장**: LLM 호출/파싱이 실패하거나 특정 대상의 관계가 판단되지 않으면
    /// 그 대상은 `RelatedTo`로 채운다. 따라서 항상 `targets` 전체에 대한 관계를
    /// 반환한다(엣지 생성이 관계 판단 실패로 누락되지 않음). 대상이 없으면 빈 벡터.
    pub async fn judge_relations(
        &self,
        llm: &dyn LlmProvider,
        source_summary: &str,
        targets: &[CandidateDoc],
    ) -> Vec<(Uuid, RelationType)> {
        if targets.is_empty() {
            return Vec::new();
        }

        let prompt = build_relation_prompt(source_summary, targets);
        let judged: HashMap<Uuid, RelationType> = match llm.complete(&prompt).await {
            Ok(resp) => parse_relations(&resp, targets).into_iter().collect(),
            Err(e) => {
                tracing::warn!("관계 판단 LLM 호출 실패, RELATED_TO 폴백: {}", e);
                HashMap::new()
            }
        };

        // 모든 대상에 관계를 부여한다 — 판단되지 않은 대상은 RELATED_TO 기본.
        targets
            .iter()
            .map(|t| {
                (
                    t.id,
                    judged.get(&t.id).copied().unwrap_or(RelationType::RelatedTo),
                )
            })
            .collect()
    }
}

/// 판단 프롬프트를 구성한다 (중앙화 — 구조를 단위 테스트로 고정).
///
/// 후보 문서는 ID와 요약만 전달해 프롬프트 비용과 지연을 억제한다.
pub fn build_ingest_decision_prompt(
    input: &str,
    candidates: &[CandidateDoc],
    max_split: usize,
) -> String {
    let candidate_block = if candidates.is_empty() {
        "관련 문서 없음".to_string()
    } else {
        candidates
            .iter()
            .map(|c| format!("- [{}] {}", c.id, c.summary))
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        r#"당신은 개인 지식 베이스의 저장 판단 에이전트입니다.
새로 들어온 정보를 기존 지식과 대조해 어떻게 저장할지 결정합니다.

## 새 입력 정보
"""
{input}
"""

## 기존 관련 문서 (유사도 상위, 요약)
{candidate_block}

## 판단할 전략
- "new": 기존과 무관한 새 정보 → 새 문서로 저장
- "update": 위 기존 문서 중 하나와 같은 주제의 갱신·추가 정보 → 그 문서를 업데이트
- "split": 서로 독립적인 여러 주제가 한 입력에 섞임 → 주제별로 분할 (최대 {max_split}개)
- "duplicate": 위 기존 문서 중 하나와 사실상 동일한 중복

## 출력 형식 (JSON만, 다른 텍스트 없이)
{{
  "strategy": "new" | "update" | "split" | "duplicate",
  "target": "update/duplicate일 때 위 목록의 대상 문서 ID(UUID). 그 외에는 null",
  "segments": ["split일 때 각 주제로 나눈 독립적 텍스트 조각들. 그 외에는 빈 배열"],
  "reason": "판단 근거를 한 문장으로"
}}

## 규칙
- target은 반드시 위 "기존 관련 문서" 목록에 실제로 있는 ID여야 합니다. 목록에 없으면 new로 판단하세요.
- split의 각 조각은 원문 내용을 보존해야 하며, 독립적으로 이해 가능해야 합니다.
- 확신이 없으면 new로 판단하세요 (안전한 기본값)."#
    )
}

/// LLM 응답 문자열을 `AgentDecision`으로 파싱한다 (순수 함수).
///
/// 안전 강등 규칙 — 정보 유실 없이 오판을 흡수한다:
/// - update/duplicate인데 target이 후보 목록에 없으면(환각) → New로 강등.
/// - split인데 유효 세그먼트가 2개 미만이면(분할 무의미) → New로 강등.
/// - strategy 키 자체가 알 수 없는 값이면 → `Err`(재시도/폴백 대상).
pub fn parse_ingest_decision(
    response: &str,
    candidates: &[CandidateDoc],
    max_split: usize,
) -> Result<AgentDecision> {
    let json_str = extract_json(response);
    let raw: RawDecision =
        serde_json::from_str(json_str).context("판단 응답을 JSON으로 파싱하지 못했습니다")?;

    let candidate_ids: HashSet<Uuid> = candidates.iter().map(|c| c.id).collect();

    // 후보 목록에 실제로 존재하는 target만 채택 (환각 방지).
    let resolved_target = raw
        .target
        .as_deref()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty() && *t != "null")
        .and_then(|t| Uuid::parse_str(t).ok())
        .filter(|id| candidate_ids.contains(id));

    let strategy = match raw.strategy.trim().to_lowercase().as_str() {
        "new" => IngestStrategy::New,
        "update" => match resolved_target {
            Some(id) => IngestStrategy::Update { target: id },
            None => IngestStrategy::New, // 환각/누락 target → 안전 강등
        },
        "duplicate" => match resolved_target {
            Some(id) => IngestStrategy::Duplicate { of: id },
            None => IngestStrategy::New,
        },
        "split" => {
            let segments: Vec<String> = raw
                .segments
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .take(max_split)
                .collect();
            if segments.len() >= 2 {
                IngestStrategy::Split { segments }
            } else {
                IngestStrategy::New // 분할 의미 없음 → 안전 강등
            }
        }
        other => anyhow::bail!("알 수 없는 전략: {}", other),
    };

    Ok(AgentDecision {
        strategy,
        reason: raw.reason,
    })
}

/// LLM이 반환하는 관계 판단 JSON의 원시 형태.
#[derive(Debug, Deserialize)]
struct RawRelations {
    #[serde(default)]
    relations: Vec<RawRelation>,
}

#[derive(Debug, Deserialize)]
struct RawRelation {
    #[serde(default)]
    target: String,
    #[serde(default)]
    relation: String,
}

/// 관계 타입 판단 프롬프트를 구성한다 (중앙화 — 구조를 단위 테스트로 고정).
pub fn build_relation_prompt(source_summary: &str, targets: &[CandidateDoc]) -> String {
    let target_block = targets
        .iter()
        .map(|t| format!("- [{}] {}", t.id, t.summary))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"두 지식 문서 사이의 관계 타입을 판단합니다.

## 출발 문서 (방금 저장된 새 문서)
{source_summary}

## 대상 문서들
{target_block}

## 관계 타입
- related_to: 일반적 연관 (주제·맥락이 겹침)
- updates: 출발 문서가 대상을 갱신·대체함
- contradicts: 출발 문서가 대상과 상충함
- references: 출발 문서가 대상을 참조·인용함
- part_of: 출발 문서가 대상의 부분임

## 출력 형식 (JSON만, 다른 텍스트 없이)
{{"relations": [{{"target": "대상 문서 ID(UUID)", "relation": "위 관계 타입 중 하나"}}]}}

각 대상 문서마다 가장 적절한 관계 하나씩. 애매하면 related_to를 쓰세요."#
    )
}

/// 관계 판단 응답을 파싱한다 (순수 함수).
///
/// 대상 목록에 실제로 있는 ID만 채택하고(환각 방지), 알 수 없는 관계 문자열은
/// `RelatedTo`로 흡수한다. 파싱 자체가 실패하면 빈 벡터를 반환해 호출 측이 전량
/// `RelatedTo`로 폴백하게 한다(엣지 유실 없음).
pub fn parse_relations(response: &str, targets: &[CandidateDoc]) -> Vec<(Uuid, RelationType)> {
    let target_ids: HashSet<Uuid> = targets.iter().map(|t| t.id).collect();
    let json_str = extract_json(response);

    let raw: RawRelations = match serde_json::from_str(json_str) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    raw.relations
        .into_iter()
        .filter_map(|r| {
            let id = Uuid::parse_str(r.target.trim()).ok()?;
            if !target_ids.contains(&id) {
                return None; // 환각 대상 무시
            }
            let rel = RelationType::from_str(&r.relation).unwrap_or(RelationType::RelatedTo);
            Some((id, rel))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ParsedContent;
    use async_trait::async_trait;
    use crate::llm::ProviderType;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    fn candidate(summary: &str) -> CandidateDoc {
        CandidateDoc {
            id: Uuid::new_v4(),
            summary: summary.to_string(),
        }
    }

    // ──── 순수 함수: 프롬프트 빌더 ────

    #[test]
    fn test_prompt_includes_input_and_candidates() {
        let cands = vec![candidate("이전 면접 기록"), candidate("Rust 학습 노트")];
        let prompt = build_ingest_decision_prompt("오늘 A사 면접", &cands, 5);

        // 필수 입력이 프롬프트에 포함되어야 한다.
        assert!(prompt.contains("오늘 A사 면접"), "입력 텍스트 누락");
        assert!(prompt.contains("이전 면접 기록"), "후보 요약 누락");
        assert!(prompt.contains(&cands[0].id.to_string()), "후보 ID 누락");
        // 전략 키워드와 분할 상한이 명시되어야 한다.
        for kw in ["new", "update", "split", "duplicate"] {
            assert!(prompt.contains(kw), "전략 키워드 '{}' 누락", kw);
        }
        assert!(prompt.contains('5'), "분할 상한 누락");
    }

    #[test]
    fn test_prompt_no_candidates() {
        let prompt = build_ingest_decision_prompt("입력", &[], 5);
        assert!(prompt.contains("관련 문서 없음"));
        assert!(prompt.contains("입력"));
    }

    // ──── 순수 함수: 응답 파서 ────

    #[test]
    fn test_parse_new() {
        let d = parse_ingest_decision(r#"{"strategy":"new","reason":"무관"}"#, &[], 5).unwrap();
        assert_eq!(d.strategy, IngestStrategy::New);
        assert_eq!(d.reason, "무관");
    }

    #[test]
    fn test_parse_update_valid_target() {
        let cands = vec![candidate("이사 준비")];
        let target = cands[0].id;
        let resp = format!(r#"{{"strategy":"update","target":"{}","reason":"동일 주제"}}"#, target);
        let d = parse_ingest_decision(&resp, &cands, 5).unwrap();
        assert_eq!(d.strategy, IngestStrategy::Update { target });
    }

    #[test]
    fn test_parse_update_hallucinated_target_degrades_to_new() {
        // 후보에 없는 target → New로 안전 강등 (정보 유실 없음).
        let cands = vec![candidate("이사 준비")];
        let ghost = Uuid::new_v4();
        let resp = format!(r#"{{"strategy":"update","target":"{}","reason":"x"}}"#, ghost);
        let d = parse_ingest_decision(&resp, &cands, 5).unwrap();
        assert_eq!(d.strategy, IngestStrategy::New, "환각 target은 New로 강등되어야 한다");
    }

    #[test]
    fn test_parse_update_null_target_degrades_to_new() {
        let resp = r#"{"strategy":"update","target":null,"reason":"x"}"#;
        let d = parse_ingest_decision(resp, &[], 5).unwrap();
        assert_eq!(d.strategy, IngestStrategy::New);
    }

    #[test]
    fn test_parse_duplicate_valid_target() {
        let cands = vec![candidate("중복 원본")];
        let of = cands[0].id;
        let resp = format!(r#"{{"strategy":"duplicate","target":"{}","reason":"동일"}}"#, of);
        let d = parse_ingest_decision(&resp, &cands, 5).unwrap();
        assert_eq!(d.strategy, IngestStrategy::Duplicate { of });
    }

    #[test]
    fn test_parse_split_two_segments() {
        let resp = r#"{"strategy":"split","segments":["면접 후기","학습 노트"],"reason":"두 주제"}"#;
        let d = parse_ingest_decision(resp, &[], 5).unwrap();
        match d.strategy {
            IngestStrategy::Split { segments } => {
                assert_eq!(segments, vec!["면접 후기", "학습 노트"]);
            }
            other => panic!("Split 기대, {:?}", other),
        }
    }

    #[test]
    fn test_parse_split_one_segment_degrades_to_new() {
        // 세그먼트 1개는 분할 의미가 없으므로 New로 강등.
        let resp = r#"{"strategy":"split","segments":["단일 주제"],"reason":"x"}"#;
        let d = parse_ingest_decision(resp, &[], 5).unwrap();
        assert_eq!(d.strategy, IngestStrategy::New);
    }

    #[test]
    fn test_parse_split_respects_max_split() {
        // segments가 상한을 넘으면 상한까지만 취한다.
        let resp = r#"{"strategy":"split","segments":["a","b","c","d"],"reason":"x"}"#;
        let d = parse_ingest_decision(resp, &[], 2).unwrap();
        match d.strategy {
            IngestStrategy::Split { segments } => assert_eq!(segments.len(), 2),
            other => panic!("Split 기대, {:?}", other),
        }
    }

    #[test]
    fn test_parse_split_filters_empty_segments() {
        // 공백 세그먼트는 제거되고, 남은 게 2개 미만이면 New로 강등.
        let resp = r#"{"strategy":"split","segments":["  ","유일"],"reason":"x"}"#;
        let d = parse_ingest_decision(resp, &[], 5).unwrap();
        assert_eq!(d.strategy, IngestStrategy::New);
    }

    #[test]
    fn test_parse_with_code_fence() {
        // ```json 코드펜스로 감싼 응답도 파싱되어야 한다.
        let resp = "```json\n{\"strategy\":\"new\",\"reason\":\"y\"}\n```";
        let d = parse_ingest_decision(resp, &[], 5).unwrap();
        assert_eq!(d.strategy, IngestStrategy::New);
    }

    #[test]
    fn test_parse_unknown_strategy_errors() {
        // 알 수 없는 전략은 Err (재시도/폴백 대상).
        assert!(parse_ingest_decision(r#"{"strategy":"frobnicate"}"#, &[], 5).is_err());
    }

    #[test]
    fn test_parse_malformed_json_errors() {
        assert!(parse_ingest_decision("not json at all", &[], 5).is_err());
    }

    // ──── mock provider: decide 분기·재시도·폴백 ────

    /// 순차적으로 미리 정한 응답을 반환하는 테스트용 LLM provider.
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

    #[tokio::test]
    async fn test_decide_new_branch() {
        let llm = MockLlm::new(vec![Ok(r#"{"strategy":"new","reason":"r"}"#.to_string())]);
        let agent = IngestAgent::new();
        let d = agent.decide(&llm, "입력", &[]).await.unwrap();
        assert_eq!(d.strategy, IngestStrategy::New);
        assert_eq!(llm.call_count(), 1);
    }

    #[tokio::test]
    async fn test_decide_update_branch() {
        let cands = vec![candidate("기존")];
        let target = cands[0].id;
        let llm = MockLlm::new(vec![Ok(format!(
            r#"{{"strategy":"update","target":"{}","reason":"r"}}"#,
            target
        ))]);
        let agent = IngestAgent::new();
        let d = agent.decide(&llm, "입력", &cands).await.unwrap();
        assert_eq!(d.strategy, IngestStrategy::Update { target });
    }

    #[tokio::test]
    async fn test_decide_split_branch() {
        let llm = MockLlm::new(vec![Ok(
            r#"{"strategy":"split","segments":["a","b"],"reason":"r"}"#.to_string(),
        )]);
        let agent = IngestAgent::new();
        let d = agent.decide(&llm, "입력", &[]).await.unwrap();
        assert!(matches!(d.strategy, IngestStrategy::Split { .. }));
    }

    #[tokio::test]
    async fn test_decide_duplicate_branch() {
        let cands = vec![candidate("원본")];
        let of = cands[0].id;
        let llm = MockLlm::new(vec![Ok(format!(
            r#"{{"strategy":"duplicate","target":"{}","reason":"r"}}"#,
            of
        ))]);
        let agent = IngestAgent::new();
        let d = agent.decide(&llm, "입력", &cands).await.unwrap();
        assert_eq!(d.strategy, IngestStrategy::Duplicate { of });
    }

    #[tokio::test]
    async fn test_decide_retries_once_then_succeeds() {
        // 첫 응답은 파싱 불가, 두 번째는 유효 → 재시도로 성공.
        let llm = MockLlm::new(vec![
            Ok("쓰레기 응답".to_string()),
            Ok(r#"{"strategy":"new","reason":"r"}"#.to_string()),
        ]);
        let agent = IngestAgent::new();
        let d = agent.decide(&llm, "입력", &[]).await.unwrap();
        assert_eq!(d.strategy, IngestStrategy::New);
        assert_eq!(llm.call_count(), 2, "재시도가 발생해야 한다");
    }

    #[tokio::test]
    async fn test_decide_all_attempts_fail_returns_err() {
        // 두 시도 모두 파싱 불가 → Err (호출 측이 raw 폴백).
        let llm = MockLlm::new(vec![
            Ok("쓰레기1".to_string()),
            Ok("쓰레기2".to_string()),
        ]);
        let agent = IngestAgent::new();
        assert!(agent.decide(&llm, "입력", &[]).await.is_err());
        assert_eq!(llm.call_count(), MAX_DECISION_ATTEMPTS);
    }

    #[tokio::test]
    async fn test_decide_llm_error_returns_err() {
        // LLM 호출 자체가 실패 → Err (폴백 대상).
        let llm = MockLlm::new(vec![Err(anyhow!("타임아웃")), Err(anyhow!("타임아웃"))]);
        let agent = IngestAgent::new();
        assert!(agent.decide(&llm, "입력", &[]).await.is_err());
    }

    // ──── 관계 타입 판단 (자동 엣지) ────

    #[test]
    fn test_relation_prompt_includes_source_and_targets() {
        let targets = vec![candidate("대상 요약")];
        let prompt = build_relation_prompt("출발 요약", &targets);
        assert!(prompt.contains("출발 요약"));
        assert!(prompt.contains("대상 요약"));
        assert!(prompt.contains(&targets[0].id.to_string()));
        for kw in ["related_to", "updates", "contradicts", "references", "part_of"] {
            assert!(prompt.contains(kw), "관계 키워드 '{}' 누락", kw);
        }
    }

    #[test]
    fn test_parse_relations_valid() {
        let targets = vec![candidate("t1"), candidate("t2")];
        let resp = format!(
            r#"{{"relations":[{{"target":"{}","relation":"updates"}},{{"target":"{}","relation":"references"}}]}}"#,
            targets[0].id, targets[1].id
        );
        let rels = parse_relations(&resp, &targets);
        assert_eq!(rels.len(), 2);
        assert!(rels.contains(&(targets[0].id, RelationType::Updates)));
        assert!(rels.contains(&(targets[1].id, RelationType::References)));
    }

    #[test]
    fn test_parse_relations_ignores_hallucinated_target() {
        let targets = vec![candidate("t1")];
        let ghost = Uuid::new_v4();
        let resp = format!(r#"{{"relations":[{{"target":"{}","relation":"updates"}}]}}"#, ghost);
        assert!(parse_relations(&resp, &targets).is_empty(), "환각 대상은 무시");
    }

    #[test]
    fn test_parse_relations_unknown_relation_becomes_related_to() {
        let targets = vec![candidate("t1")];
        let resp = format!(r#"{{"relations":[{{"target":"{}","relation":"weird"}}]}}"#, targets[0].id);
        let rels = parse_relations(&resp, &targets);
        assert_eq!(rels, vec![(targets[0].id, RelationType::RelatedTo)]);
    }

    #[test]
    fn test_parse_relations_malformed_is_empty() {
        assert!(parse_relations("garbage", &[candidate("t")]).is_empty());
    }

    #[tokio::test]
    async fn test_judge_relations_success() {
        let targets = vec![candidate("t1")];
        let resp = format!(
            r#"{{"relations":[{{"target":"{}","relation":"updates"}}]}}"#,
            targets[0].id
        );
        let llm = MockLlm::new(vec![Ok(resp)]);
        let agent = IngestAgent::new();
        let rels = agent.judge_relations(&llm, "출발", &targets).await;
        assert_eq!(rels, vec![(targets[0].id, RelationType::Updates)]);
    }

    #[tokio::test]
    async fn test_judge_relations_llm_failure_falls_back_to_related_to() {
        // LLM 실패 시 모든 대상이 RELATED_TO로 채워져야 한다 (엣지 유실 없음).
        let targets = vec![candidate("t1"), candidate("t2")];
        let llm = MockLlm::new(vec![Err(anyhow!("장애"))]);
        let agent = IngestAgent::new();
        let rels = agent.judge_relations(&llm, "출발", &targets).await;
        assert_eq!(rels.len(), 2);
        assert!(rels.iter().all(|(_, r)| *r == RelationType::RelatedTo));
    }

    #[tokio::test]
    async fn test_judge_relations_unjudged_target_defaults_related_to() {
        // 일부만 판단되면 나머지는 RELATED_TO로 채워진다.
        let targets = vec![candidate("t1"), candidate("t2")];
        let resp = format!(
            r#"{{"relations":[{{"target":"{}","relation":"part_of"}}]}}"#,
            targets[0].id
        );
        let llm = MockLlm::new(vec![Ok(resp)]);
        let agent = IngestAgent::new();
        let rels = agent.judge_relations(&llm, "출발", &targets).await;
        assert_eq!(rels.len(), 2);
        assert!(rels.contains(&(targets[0].id, RelationType::PartOf)));
        assert!(rels.contains(&(targets[1].id, RelationType::RelatedTo)));
    }

    #[tokio::test]
    async fn test_judge_relations_empty_targets() {
        let llm = MockLlm::new(vec![]);
        let agent = IngestAgent::new();
        let rels = agent.judge_relations(&llm, "출발", &[]).await;
        assert!(rels.is_empty());
        assert_eq!(llm.call_count(), 0, "대상이 없으면 LLM을 호출하지 않아야 한다");
    }
}
