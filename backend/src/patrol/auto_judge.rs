//! Patrol auto-judge — `review_mode=auto`일 때 Review Queue의 열린 항목을 AI가 판정한다.
//!
//! 이 모듈은 **판정(decision)만** 담당하는 순수 로직이다(프롬프트 빌드·응답 파싱·가드).
//! 실제 반영(freshness 갱신·soft delete·소스 재유입)과 항목 순회는 [`crate::patrol::Patrol`]
//! 오케스트레이터가 조율한다 — auto_judge는 저장소/LLM/executor를 알지 못한다.
//!
//! **설계 안전장치(유저 지시로 "삭제는 사람만" 불변식을 완화하되 위험은 코드로 봉인):**
//! - **soft delete만**: 삭제 판정은 기존 judge(Deleted) 경로 → `soft_delete_document`(버전
//!   보관 후 삭제). 하드 삭제 경로는 auto가 접근하지 않는다.
//! - **실패는 전부 Pending 잔류**: 허용 목록 밖 판정·JSON 파싱 실패·LLM 오류는 판정을
//!   만들지 않고(`Err`) 오케스트레이터가 항목을 열린 상태로 남긴다(절대 실패를 삭제로
//!   강등하지 않는다). ingest_agent의 환각 강등 패턴과 동형이다.
//! - **판단 근거·주체 영구 기록**: 판정은 `decided_by=auto`·`decision_reason`으로 각인.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::llm::extract_json;
use crate::patrol::detectors::DetectorKind;
use crate::patrol::review::ReviewDecision;

/// auto 모드 한 실행당 자동 판정 상한 기본값(설정 `auto_judge_cap` 기본과 일치 — LLM
/// 호출 수를 실행당 상수로 바운드, policy `[Backend] - [LLM]`).
pub const DEFAULT_AUTO_JUDGE_CAP: usize = 50;

/// LLM 프롬프트에 넣을 문서 원문 발췌 상한(문자). 비용·지연 억제 — 요약과 함께 제공한다.
pub const CONTENT_EXCERPT_CHARS: usize = 1_200;

/// 유형별 **허용 판정** — 이 목록 밖 판정은 파싱에서 거부되어 항목이 Pending에 잔류한다.
///
/// - `staleness`: valid(유예) | deleted(제거) | needs_fix(사람 손질 필요)
/// - `duplicate`: deleted(중복 제거 — 플래그된 최신 문서) | dismissed(오탐)
/// - `orphan`: valid(유효) | deleted(제거) | dismissed(오탐)
/// - `external_mismatch`: LLM 판정 대상이 아니다(소스 재유입으로 해소) → 빈 목록.
pub fn allowed_decisions(kind: DetectorKind) -> &'static [ReviewDecision] {
    use ReviewDecision::*;
    match kind {
        DetectorKind::Staleness => &[Valid, Deleted, NeedsFix],
        DetectorKind::Duplicate => &[Deleted, Dismissed],
        DetectorKind::Orphan => &[Valid, Deleted, Dismissed],
        DetectorKind::ExternalMismatch => &[],
    }
}

/// 판정별 의미 설명(프롬프트에 그대로 삽입 — LLM이 각 선택지의 부수효과를 이해하도록).
fn decision_meaning(d: ReviewDecision) -> &'static str {
    match d {
        ReviewDecision::Valid => "valid: 문서가 여전히 유효하다(당분간 staleness 유예). 확신이 없으면 이쪽으로 보수적으로.",
        ReviewDecision::NeedsFix => "needs_fix: 내용이 낡았지만 삭제하기엔 아까워 사람이 직접 손질해야 한다.",
        ReviewDecision::Deleted => "deleted: 원장에서 제거한다(버전은 보관되어 복구 가능). 되살릴 수 있으니 명백한 경우에 선택.",
        ReviewDecision::Dismissed => "dismissed: 탐지가 오탐이다(문제 없음). 조치 없이 닫는다.",
    }
}

/// LLM 판정 응답 JSON의 원시 형태.
#[derive(Debug, Deserialize)]
struct RawAutoJudgment {
    decision: String,
    #[serde(default)]
    reason: String,
}

/// 파싱·가드를 통과한 auto 판정.
#[derive(Debug, Clone, PartialEq)]
pub struct AutoJudgment {
    pub decision: ReviewDecision,
    pub reason: String,
}

/// 한 번의 auto-judge 패스 결과 요약 — 관측용(run 로그·PatrolRun 기록).
///
/// `#[serde(default)]` + `Option` 래핑(PatrolRun 쪽)으로 기존 이력 JSON과 하위호환된다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoJudgeSummary {
    /// 이번 패스에서 판정을 시도한 열린 항목 수(cap 상한 이내).
    pub processed: usize,
    pub valid: usize,
    pub needs_fix: usize,
    pub deleted: usize,
    pub dismissed: usize,
    /// external_mismatch 소스 재유입으로 해소된 항목 수.
    pub reingested: usize,
    /// 파싱/LLM/재유입 실패로 **Pending에 잔류**시킨 항목 수(안전 폴백).
    pub failed: usize,
}

impl AutoJudgeSummary {
    /// 실제로 닫힌(반영된) 항목 수 — 요약 로그용.
    pub fn resolved(&self) -> usize {
        self.valid + self.needs_fix + self.deleted + self.dismissed + self.reingested
    }
}

/// 문자 경계 안전 발췌 — 원문 앞부분을 최대 `max_chars`자까지, 잘리면 `…`를 붙인다.
pub fn content_excerpt(raw: &str, max_chars: usize) -> String {
    let mut out: String = raw.chars().take(max_chars).collect();
    if raw.chars().count() > max_chars {
        out.push('…');
    }
    out
}

/// auto 판정 프롬프트를 구성한다(중앙화 — 구조를 단위 테스트로 고정).
///
/// 대상 문서 요약 + 원문 발췌 + 탐지 근거 수치를 주고, 유형별 **허용 판정만** 고르게 한다.
/// duplicate는 상대 문서(`other_summary`) 요약도 제공해 무엇을 남길지 판단하게 한다.
pub fn build_auto_judge_prompt(
    kind: DetectorKind,
    reason: &str,
    evidence: &serde_json::Value,
    primary_summary: &str,
    primary_excerpt: &str,
    other_summary: Option<&str>,
) -> String {
    let allowed = allowed_decisions(kind);
    let options_block = allowed
        .iter()
        .map(|d| format!("- {}", decision_meaning(*d)))
        .collect::<Vec<_>>()
        .join("\n");
    let allowed_literal = allowed
        .iter()
        .map(|d| format!("\"{}\"", d.as_str()))
        .collect::<Vec<_>>()
        .join(" | ");

    let evidence_str =
        serde_json::to_string(evidence).unwrap_or_else(|_| "{}".to_string());

    let other_block = match other_summary {
        Some(s) => format!("\n## 유사(중복 의심) 상대 문서 요약\n{s}\n"),
        None => String::new(),
    };

    format!(
        r#"당신은 개인 지식 원장 "Maia"의 자기 관리(Patrol) 판정 에이전트입니다.
Patrol이 아래 문서를 '{kind}' 유형으로 플래그했습니다. 이 문서를 어떻게 처리할지 판정하세요.

## 탐지 사유
{reason}

## 탐지 근거 수치
{evidence_str}

## 대상 문서 요약
{primary_summary}

## 대상 문서 원문(발췌)
"""
{primary_excerpt}
"""
{other_block}
## 선택 가능한 판정(이 중에서만 고르세요)
{options_block}

## 판정 원칙
- 소유자의 기억을 책임지는 시스템입니다. **삭제(deleted)는 명백할 때만** 고르고, 애매하면
  위 선택지 중 보존하는 쪽을 택하세요. 삭제해도 버전은 보관되지만 신중히 판단하세요.
- 원문에 없는 사실을 지어내지 말고, 위 근거와 내용만으로 판단하세요.

## 출력 형식 (JSON만, 다른 텍스트 없이)
{{
  "decision": {allowed_literal},
  "reason": "판정 근거를 한 문장으로(한국어)"
}}"#,
        kind = kind.as_str(),
    )
}

/// LLM 응답을 `AutoJudgment`로 파싱한다(순수 함수, 환각·범위 가드 포함).
///
/// **가드(실패 → Err → 호출 측이 Pending 유지):**
/// - JSON 파싱 불가 → `Err`.
/// - decision 문자열을 판정으로 못 읽음 → `Err`.
/// - 판정이 해당 유형의 [`allowed_decisions`] 밖 → `Err`(범위 밖 판정 거부).
///
/// 어떤 경우도 "삭제로 강등"하지 않는다 — 불확실은 항상 안전한 쪽(항목 잔류)으로 흡수된다.
pub fn parse_auto_judgment(response: &str, kind: DetectorKind) -> Result<AutoJudgment> {
    let json_str = extract_json(response);
    let raw: RawAutoJudgment =
        serde_json::from_str(json_str).context("auto 판정 응답을 JSON으로 파싱하지 못했습니다")?;

    let decision = parse_decision_str(&raw.decision)
        .ok_or_else(|| anyhow!("알 수 없는 판정 문자열: '{}'", raw.decision))?;

    if !allowed_decisions(kind).contains(&decision) {
        return Err(anyhow!(
            "유형 '{}'에 허용되지 않은 판정 '{}' — Pending 유지",
            kind.as_str(),
            decision.as_str()
        ));
    }

    Ok(AutoJudgment {
        decision,
        reason: raw.reason.trim().to_string(),
    })
}

/// 판정 문자열을 [`ReviewDecision`]으로 관대하게 매핑한다(모르는 값은 None).
fn parse_decision_str(s: &str) -> Option<ReviewDecision> {
    match s.trim().to_lowercase().as_str() {
        "valid" => Some(ReviewDecision::Valid),
        "needs_fix" | "needsfix" | "needs-fix" => Some(ReviewDecision::NeedsFix),
        "deleted" | "delete" => Some(ReviewDecision::Deleted),
        "dismissed" | "dismiss" => Some(ReviewDecision::Dismissed),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ──── 허용 목록 ────

    #[test]
    fn test_allowed_decisions_per_kind() {
        assert!(allowed_decisions(DetectorKind::Staleness).contains(&ReviewDecision::NeedsFix));
        assert!(allowed_decisions(DetectorKind::Staleness).contains(&ReviewDecision::Deleted));
        // duplicate는 valid/needs_fix를 허용하지 않는다(중복은 제거하거나 오탐 기각만).
        assert!(!allowed_decisions(DetectorKind::Duplicate).contains(&ReviewDecision::Valid));
        assert!(!allowed_decisions(DetectorKind::Duplicate).contains(&ReviewDecision::NeedsFix));
        assert!(allowed_decisions(DetectorKind::Duplicate).contains(&ReviewDecision::Deleted));
        // orphan은 needs_fix 없음(고아는 유효/삭제/오탐).
        assert!(!allowed_decisions(DetectorKind::Orphan).contains(&ReviewDecision::NeedsFix));
        // external_mismatch는 LLM 판정 대상 아님.
        assert!(allowed_decisions(DetectorKind::ExternalMismatch).is_empty());
    }

    // ──── 발췌 ────

    #[test]
    fn test_content_excerpt_truncates_with_ellipsis() {
        let s = "가나다라마바사";
        assert_eq!(content_excerpt(s, 3), "가나다…");
        assert_eq!(content_excerpt(s, 100), "가나다라마바사", "짧으면 원문 그대로");
        assert_eq!(content_excerpt("", 5), "");
    }

    // ──── 프롬프트 빌더 ────

    #[test]
    fn test_prompt_includes_kind_reason_and_allowed_only() {
        let ev = json!({"age_days": 400, "threshold_days": 197});
        let prompt = build_auto_judge_prompt(
            DetectorKind::Staleness,
            "400일 미갱신",
            &ev,
            "리액트 훅 정리",
            "본문 발췌...",
            None,
        );
        assert!(prompt.contains("staleness"));
        assert!(prompt.contains("400일 미갱신"), "탐지 사유 포함");
        assert!(prompt.contains("리액트 훅 정리"), "요약 포함");
        assert!(prompt.contains("age_days"), "근거 수치 포함");
        // staleness 허용: valid/deleted/needs_fix 포함, dismissed는 미포함.
        assert!(prompt.contains("valid"));
        assert!(prompt.contains("needs_fix"));
        assert!(prompt.contains("deleted"));
        assert!(!prompt.contains("dismissed"), "staleness에는 dismissed 선택지 없음");
    }

    #[test]
    fn test_prompt_duplicate_includes_other_summary() {
        let ev = json!({"similarity": 0.95});
        let prompt = build_auto_judge_prompt(
            DetectorKind::Duplicate,
            "유사도 0.95",
            &ev,
            "새 문서 요약",
            "본문",
            Some("원본 문서 요약"),
        );
        assert!(prompt.contains("원본 문서 요약"), "중복 상대 문서 요약 포함");
        assert!(prompt.contains("dismissed"));
        assert!(prompt.contains("deleted"));
        assert!(!prompt.contains("\"valid\""), "duplicate에는 valid 선택지 없음");
    }

    // ──── 응답 파서 + 가드 ────

    #[test]
    fn test_parse_valid_decision() {
        let d = parse_auto_judgment(r#"{"decision":"deleted","reason":"명백한 중복"}"#, DetectorKind::Duplicate).unwrap();
        assert_eq!(d.decision, ReviewDecision::Deleted);
        assert_eq!(d.reason, "명백한 중복");
    }

    #[test]
    fn test_parse_with_code_fence() {
        let resp = "```json\n{\"decision\":\"valid\",\"reason\":\"아직 유효\"}\n```";
        let d = parse_auto_judgment(resp, DetectorKind::Staleness).unwrap();
        assert_eq!(d.decision, ReviewDecision::Valid);
    }

    #[test]
    fn test_parse_rejects_out_of_allowlist_decision() {
        // duplicate에 valid는 허용되지 않는다 → Err(항목 Pending 유지).
        let err = parse_auto_judgment(r#"{"decision":"valid","reason":"x"}"#, DetectorKind::Duplicate);
        assert!(err.is_err(), "허용 목록 밖 판정은 거부되어야 한다");
    }

    #[test]
    fn test_parse_rejects_unknown_decision_string() {
        let err = parse_auto_judgment(r#"{"decision":"frobnicate","reason":"x"}"#, DetectorKind::Orphan);
        assert!(err.is_err());
    }

    #[test]
    fn test_parse_rejects_malformed_json() {
        assert!(parse_auto_judgment("not json", DetectorKind::Staleness).is_err());
    }

    #[test]
    fn test_parse_never_degrades_to_delete_on_failure() {
        // 실패 경로는 절대 Deleted를 반환하지 않는다 — Err로 흡수되어 호출 측이 Pending 유지.
        for bad in ["garbage", r#"{"decision":"???"}"#, r#"{"reason":"no decision"}"#] {
            assert!(parse_auto_judgment(bad, DetectorKind::Staleness).is_err());
        }
    }

    #[test]
    fn test_summary_resolved_count() {
        let s = AutoJudgeSummary {
            processed: 5,
            valid: 1,
            deleted: 2,
            reingested: 1,
            failed: 1,
            ..Default::default()
        };
        assert_eq!(s.resolved(), 4, "닫힌 항목 = valid+deleted+reingested");
    }
}
