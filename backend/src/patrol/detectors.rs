//! Patrol 탐지기 — staleness / 중복 / 고아 / 외부 불일치.
//!
//! **설계 원칙 (PRD 불변식):**
//! - 탐지기는 **LLM 없이 수치 신호 기반**으로 동작한다(비용·오탐 제어). 각 탐지기는
//!   미리 수집된 [`DocSignal`] 목록을 소비하는 **순수 함수**라 mock 없이 단위 테스트된다.
//! - 탐지 임계값은 워크스페이스 patrol `strictness`에서 파생된다([`Thresholds`]).
//! - 각 탐지기는 독립적이며, 하나의 실패가 다른 탐지기를 막지 않는다
//!   ([`combine_detector_results`]가 실패를 격리한다).
//!
//! 탐지기는 **수정하지 않는다** — 후보([`ReviewCandidate`])만 만들어 사람 판단(Review
//! Queue)에 넘긴다.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

/// 탐지 유형 — Review Queue 항목의 분류이자 dedup 키의 일부.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectorKind {
    /// 오래되고 검색에서 밀려난(피드백 포함) 문서.
    Staleness,
    /// 다른 문서와 유사도가 높은 중복 후보.
    Duplicate,
    /// 어떤 문서와도 연결되지 않은 고립 문서.
    Orphan,
    /// 커넥터 소스가 유입 이후 수정되어 문서가 뒤처진 상태.
    ExternalMismatch,
}

impl DetectorKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            DetectorKind::Staleness => "staleness",
            DetectorKind::Duplicate => "duplicate",
            DetectorKind::Orphan => "orphan",
            DetectorKind::ExternalMismatch => "external_mismatch",
        }
    }
}

/// 커넥터 출처 신호(외부 불일치 탐지용).
#[derive(Debug, Clone)]
pub struct SourceSignal {
    pub source_id: String,
    /// 문서에 기록된 소스 수정 시각(마지막 유입 시점).
    pub recorded_modified_at: DateTime<Utc>,
    /// 현재 소스의 실제 수정 시각(파일 stat 등). 확인 불가 시 None → 판단 보류.
    pub current_modified_at: Option<DateTime<Utc>>,
}

/// 탐지기가 소비하는, 한 문서에 대한 신호 묶음 (순수 — I/O 없음).
///
/// 오케스트레이터가 문서 raw JSON·피드백 집계·소스 stat을 모아 이 구조로 만든 뒤
/// 탐지기에 넘긴다. 탐지기 자체는 파일/네트워크에 접근하지 않아 결정적이다.
#[derive(Debug, Clone)]
pub struct DocSignal {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// "유효" 판단으로 갱신된 freshness 기준점(없으면 updated_at을 기준으로 나이 계산).
    pub freshness_checked_at: Option<DateTime<Utc>>,
    /// 이 문서에서 나가는 엣지 수(고아 판단).
    pub edge_count: usize,
    /// 요약 텍스트(중복 lexical 유사도용).
    pub summary: String,
    /// 이 문서에 대한 "관련 없음" 피드백 수(staleness 가중 신호).
    pub negative_feedback: usize,
    /// 커넥터 출처 신호(외부 불일치용). 수동 입력 문서는 None.
    pub source: Option<SourceSignal>,
}

impl DocSignal {
    /// staleness 나이 기준점: "유효" 판단이 있으면 그 시각, 없으면 updated_at.
    /// "유효" 판단이 기준점을 갱신해 당분간 다시 플래그되지 않게 하는 유예의 근거다.
    pub fn freshness_base(&self) -> DateTime<Utc> {
        self.freshness_checked_at.unwrap_or(self.updated_at)
    }
}

/// 탐지 후보 — Review Queue에 쌓일 항목의 씨앗(문서 ID + 유형 + 사유 + 근거 수치).
#[derive(Debug, Clone, PartialEq)]
pub struct ReviewCandidate {
    pub document_id: Uuid,
    pub kind: DetectorKind,
    pub reason: String,
    /// 근거 수치(JSON). Review 항목에 그대로 보존돼 사람이 판단 근거를 확인한다.
    pub evidence: serde_json::Value,
}

/// 워크스페이스 patrol `strictness`에서 파생되는 탐지 임계값.
///
/// 방향: strictness가 높을수록(엄격) 더 많이 플래그한다 — 나이·유예 임계는 짧아지고,
/// 중복 유사도 임계는 낮아진다. 기본값은 **보수적**(느슨)이라 큐가 쓰레기장이 되지 않는다
/// (PRD 리스크 완화: strictness 기본값 보수적).
#[derive(Debug, Clone)]
pub struct Thresholds {
    /// staleness로 플래그할 나이(일). 느슨 365 ~ 엄격 30.
    pub stale_age_days: f64,
    /// "관련 없음" 피드백 1건이 유효 나이를 며칠어치 늘리는지(staleness 가중, 단순).
    pub feedback_age_days: f64,
    /// 고아 판단 유예(일) — 이보다 어린(엣지 없는) 문서는 아직 플래그하지 않는다.
    /// 느슨 90 ~ 엄격 14. 갓 추가된 고립 문서의 오탐을 막는다.
    pub orphan_grace_days: f64,
    /// 중복으로 볼 최소 lexical 유사도(Jaccard). 느슨 0.9 ~ 엄격 0.6.
    pub duplicate_similarity: f32,
}

impl Thresholds {
    /// strictness(0.0 느슨 ~ 1.0 엄격)에서 임계값을 선형 보간한다.
    pub fn from_strictness(strictness: f32) -> Self {
        let s = strictness.clamp(0.0, 1.0) as f64;
        Self {
            stale_age_days: 365.0 - s * (365.0 - 30.0),
            feedback_age_days: 30.0,
            orphan_grace_days: 90.0 - s * (90.0 - 14.0),
            duplicate_similarity: (0.9 - s * 0.3) as f32,
        }
    }
}

/// (a) staleness — 나이 + 검색 미적중(피드백) 기간 기준.
///
/// freshness 기준점으로부터의 나이에, "관련 없음" 피드백을 나이 가중으로 더한 **유효
/// 나이**가 임계를 넘으면 후보로 만든다. 근거(나이·피드백·유효 나이·임계)를 항목에 남긴다.
pub fn detect_stale(signals: &[DocSignal], now: DateTime<Utc>, t: &Thresholds) -> Vec<ReviewCandidate> {
    let mut out = Vec::new();
    for s in signals {
        let age_days = ((now - s.freshness_base()).num_seconds() as f64 / 86_400.0).max(0.0);
        // 피드백이 노화를 가속한다("관련 없다"는 신호를 나이로 환산).
        let effective = age_days + s.negative_feedback as f64 * t.feedback_age_days;
        if effective >= t.stale_age_days {
            out.push(ReviewCandidate {
                document_id: s.id,
                kind: DetectorKind::Staleness,
                reason: format!(
                    "{:.0}일 미갱신 · 피드백 {}건 → 유효 나이 {:.0}일 ≥ 임계 {:.0}일",
                    age_days, s.negative_feedback, effective, t.stale_age_days
                ),
                evidence: json!({
                    "age_days": age_days.round(),
                    "negative_feedback": s.negative_feedback,
                    "effective_age_days": effective.round(),
                    "threshold_days": t.stale_age_days.round(),
                }),
            });
        }
    }
    out
}

/// (b) 중복 — 유사도 상위 쌍.
///
/// 요약 토큰의 Jaccard 유사도가 임계 이상인 쌍을 찾아, 쌍당 하나의 후보를 **더 최근**
/// 문서에 건다(나중 중복 의심 — 무엇을 남길지는 사람이 판단). `max_pairs`로 폭주를 막는다.
///
/// **벡터가 아니라 lexical을 쓰는 이유:** 탐지기는 임베딩 provider/Qdrant 없이 순수·결정적
/// 으로 동작해야 하고(단위 테스트·환경 독립), 실제 중복의 지배적 사례(동일 내용 재유입)는
/// 요약이 거의 동일해 lexical로 충분히 잡힌다. 정밀도는 사람 판단(Review Queue)이 보증한다.
pub fn detect_duplicates(
    signals: &[DocSignal],
    t: &Thresholds,
    max_pairs: usize,
) -> Vec<ReviewCandidate> {
    let mut out = Vec::new();
    if max_pairs == 0 {
        return out;
    }
    for i in 0..signals.len() {
        for j in (i + 1)..signals.len() {
            let sim = summary_similarity(&signals[i].summary, &signals[j].summary);
            if sim >= t.duplicate_similarity {
                let (flag, other) = pick_duplicate_target(&signals[i], &signals[j]);
                out.push(ReviewCandidate {
                    document_id: flag,
                    kind: DetectorKind::Duplicate,
                    reason: format!("문서 {other} 와(과) 유사도 {sim:.2} — 중복 후보"),
                    evidence: json!({
                        "similar_to": other.to_string(),
                        "similarity": sim,
                        "threshold": t.duplicate_similarity,
                    }),
                });
                if out.len() >= max_pairs {
                    return out;
                }
            }
        }
    }
    out
}

/// (c) 고아 — 엣지 없는 문서(단, 유예 기간 경과분만).
///
/// 갓 추가된 고립 문서(아직 연결이 안 붙었을 수 있음)의 오탐을 막기 위해, 생성 후
/// `orphan_grace_days`가 지난 엣지 없는 문서만 후보로 만든다.
pub fn detect_orphans(signals: &[DocSignal], now: DateTime<Utc>, t: &Thresholds) -> Vec<ReviewCandidate> {
    let mut out = Vec::new();
    for s in signals {
        if s.edge_count != 0 {
            continue;
        }
        let age_days = ((now - s.created_at).num_seconds() as f64 / 86_400.0).max(0.0);
        if age_days >= t.orphan_grace_days {
            out.push(ReviewCandidate {
                document_id: s.id,
                kind: DetectorKind::Orphan,
                reason: format!("연결된 문서 없음 · 생성 {age_days:.0}일 경과"),
                evidence: json!({
                    "edge_count": 0,
                    "age_days": age_days.round(),
                    "grace_days": t.orphan_grace_days.round(),
                }),
            });
        }
    }
    out
}

/// (d) 외부 불일치 — 커넥터 소스 수정 시각 > 문서 유입 시각.
///
/// 소스 파일이 문서 유입 이후 수정됐는데 아직 재유입되지 않은 문서를 찾는다. 현재 소스
/// 수정 시각을 확인할 수 없으면(stat 실패 등) 판단을 보류한다(오탐 방지).
pub fn detect_external_mismatch(signals: &[DocSignal]) -> Vec<ReviewCandidate> {
    let mut out = Vec::new();
    for s in signals {
        let Some(src) = &s.source else { continue };
        let Some(current) = src.current_modified_at else {
            continue;
        };
        if current > src.recorded_modified_at {
            out.push(ReviewCandidate {
                document_id: s.id,
                kind: DetectorKind::ExternalMismatch,
                reason: format!("소스 '{}'가 유입 이후 수정됨 — 문서가 소스에 뒤처짐", src.source_id),
                evidence: json!({
                    "source_id": src.source_id,
                    "recorded_modified_at": src.recorded_modified_at.to_rfc3339(),
                    "current_modified_at": current.to_rfc3339(),
                }),
            });
        }
    }
    out
}

/// 탐지기별 결과(성공/실패)를 합친다 — 실패한 탐지기는 건너뛰고 나머지는 유지한다.
///
/// PRD 불변식: "각 탐지기는 독립 실행되고 하나의 실패가 다른 탐지기를 막지 않는다."
/// 반환: (합쳐진 후보, 실패한 탐지기 유형 목록 — 관측용).
pub fn combine_detector_results(
    results: Vec<(DetectorKind, anyhow::Result<Vec<ReviewCandidate>>)>,
) -> (Vec<ReviewCandidate>, Vec<DetectorKind>) {
    let mut candidates = Vec::new();
    let mut failed = Vec::new();
    for (kind, res) in results {
        match res {
            Ok(mut c) => candidates.append(&mut c),
            Err(e) => {
                tracing::warn!("탐지기 {} 실패(격리 — 다른 탐지기는 계속): {}", kind.as_str(), e);
                failed.push(kind);
            }
        }
    }
    (candidates, failed)
}

/// 요약 토큰 집합의 Jaccard 유사도 (순수). 검색과 동일한 토크나이저를 재사용한다.
fn summary_similarity(a: &str, b: &str) -> f32 {
    let ta = token_set(a);
    let tb = token_set(b);
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let inter = ta.intersection(&tb).count();
    let union = ta.union(&tb).count();
    if union == 0 {
        0.0
    } else {
        inter as f32 / union as f32
    }
}

fn token_set(text: &str) -> HashSet<String> {
    crate::core::tokenize(text).into_iter().collect()
}

/// 중복 쌍에서 플래그할 문서를 결정한다(결정적). 더 최근에 생성된 쪽을 플래그하고
/// 나머지를 참조로 남긴다. 생성 시각이 같으면 id가 큰 쪽을 플래그한다.
fn pick_duplicate_target(a: &DocSignal, b: &DocSignal) -> (Uuid, Uuid) {
    if a.created_at > b.created_at || (a.created_at == b.created_at && a.id > b.id) {
        (a.id, b.id)
    } else {
        (b.id, a.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    fn days_ago(days: i64) -> DateTime<Utc> {
        Utc::now() - chrono::Duration::days(days)
    }

    fn signal(summary: &str) -> DocSignal {
        DocSignal {
            id: Uuid::new_v4(),
            created_at: days_ago(1),
            updated_at: days_ago(1),
            freshness_checked_at: None,
            edge_count: 1,
            summary: summary.to_string(),
            negative_feedback: 0,
            source: None,
        }
    }

    // ──── Thresholds (strictness 파생) ────

    #[test]
    fn test_thresholds_strict_flags_more() {
        let loose = Thresholds::from_strictness(0.0);
        let strict = Thresholds::from_strictness(1.0);
        assert!(strict.stale_age_days < loose.stale_age_days, "엄격할수록 나이 임계 짧음");
        assert!(strict.orphan_grace_days < loose.orphan_grace_days, "엄격할수록 유예 짧음");
        assert!(
            strict.duplicate_similarity < loose.duplicate_similarity,
            "엄격할수록 중복 유사도 임계 낮음"
        );
    }

    #[test]
    fn test_thresholds_clamps_out_of_range_strictness() {
        // 범위를 벗어난 strictness도 안전하게 클램프된다.
        let over = Thresholds::from_strictness(5.0);
        let one = Thresholds::from_strictness(1.0);
        assert!((over.stale_age_days - one.stale_age_days).abs() < 1e-9);
    }

    // ──── staleness ────

    #[test]
    fn test_stale_flags_old_document() {
        let t = Thresholds::from_strictness(0.5); // 임계 ≈ 197.5일
        let mut s = signal("old doc");
        s.updated_at = days_ago(300);
        let out = detect_stale(&[s], now(), &t);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, DetectorKind::Staleness);
    }

    #[test]
    fn test_stale_ignores_fresh_document() {
        let t = Thresholds::from_strictness(0.5);
        let mut s = signal("fresh");
        s.updated_at = days_ago(10);
        assert!(detect_stale(&[s], now(), &t).is_empty(), "최근 문서는 플래그 안 함");
    }

    #[test]
    fn test_stale_freshness_checkpoint_suspends_flag() {
        // updated_at은 오래됐어도 "유효" 판단(freshness_checked_at)이 최근이면 유예된다.
        let t = Thresholds::from_strictness(0.5);
        let mut s = signal("validated");
        s.updated_at = days_ago(300);
        s.freshness_checked_at = Some(days_ago(5));
        assert!(detect_stale(&[s], now(), &t).is_empty(), "유효 판단이 최근이면 플래그 유예");
    }

    #[test]
    fn test_stale_feedback_accelerates() {
        // 나이만으로는 임계 미만이어도, 피드백이 유효 나이를 밀어올려 플래그된다.
        let t = Thresholds::from_strictness(0.5); // 임계 ≈ 197.5일, 피드백 30일/건
        let mut s = signal("disliked");
        s.updated_at = days_ago(180); // 180 < 197.5
        s.negative_feedback = 2; // +60 → 240 ≥ 197.5
        let out = detect_stale(&[s], now(), &t);
        assert_eq!(out.len(), 1, "피드백이 노화를 가속해 플래그되어야 한다");
        assert_eq!(out[0].evidence["negative_feedback"], 2);
    }

    // ──── 중복 ────

    #[test]
    fn test_duplicate_flags_identical_summaries() {
        let t = Thresholds::from_strictness(0.5); // 유사도 임계 0.75
        let mut a = signal("리액트 훅과 상태 관리 정리");
        let mut b = signal("리액트 훅과 상태 관리 정리");
        a.created_at = days_ago(10);
        b.created_at = days_ago(2); // b가 더 최근 → b가 플래그됨
        let out = detect_duplicates(&[a.clone(), b.clone()], &t, 100);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].document_id, b.id, "더 최근 문서를 플래그");
        assert_eq!(out[0].evidence["similar_to"], a.id.to_string());
    }

    #[test]
    fn test_duplicate_ignores_dissimilar() {
        let t = Thresholds::from_strictness(0.5);
        let a = signal("리액트 훅 정리");
        let b = signal("쿠버네티스 배포 파이프라인");
        assert!(detect_duplicates(&[a, b], &t, 100).is_empty(), "무관한 요약은 중복 아님");
    }

    #[test]
    fn test_duplicate_respects_max_pairs() {
        let t = Thresholds::from_strictness(0.5);
        // 동일 요약 4개 → 쌍이 여럿이지만 상한 1로 잘린다.
        let docs: Vec<DocSignal> = (0..4).map(|_| signal("동일한 요약 텍스트")).collect();
        let out = detect_duplicates(&docs, &t, 1);
        assert_eq!(out.len(), 1, "max_pairs 상한을 지켜야 한다");
    }

    #[test]
    fn test_duplicate_empty_summary_not_similar() {
        let t = Thresholds::from_strictness(1.0); // 임계 0.6
        let a = signal("");
        let b = signal("");
        assert!(detect_duplicates(&[a, b], &t, 100).is_empty(), "빈 요약은 유사도 0");
    }

    // ──── 고아 ────

    #[test]
    fn test_orphan_flags_edgeless_aged() {
        let t = Thresholds::from_strictness(0.5); // 유예 ≈ 52일
        let mut s = signal("isolated");
        s.edge_count = 0;
        s.created_at = days_ago(100);
        let out = detect_orphans(&[s], now(), &t);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, DetectorKind::Orphan);
    }

    #[test]
    fn test_orphan_grace_protects_new_edgeless() {
        let t = Thresholds::from_strictness(0.5);
        let mut s = signal("new isolated");
        s.edge_count = 0;
        s.created_at = days_ago(3); // 유예(≈52일) 이내
        assert!(detect_orphans(&[s], now(), &t).is_empty(), "유예 내 신규 고립은 플래그 안 함");
    }

    #[test]
    fn test_orphan_ignores_connected() {
        let t = Thresholds::from_strictness(0.5);
        let mut s = signal("connected");
        s.edge_count = 2;
        s.created_at = days_ago(300);
        assert!(detect_orphans(&[s], now(), &t).is_empty(), "엣지 있으면 고아 아님");
    }

    // ──── 외부 불일치 ────

    #[test]
    fn test_mismatch_flags_stale_source_doc() {
        let mut s = signal("from connector");
        s.source = Some(SourceSignal {
            source_id: "/notes/a.md".to_string(),
            recorded_modified_at: days_ago(10),
            current_modified_at: Some(days_ago(1)), // 소스가 더 최근
        });
        let out = detect_external_mismatch(&[s]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, DetectorKind::ExternalMismatch);
        assert_eq!(out[0].evidence["source_id"], "/notes/a.md");
    }

    #[test]
    fn test_mismatch_ignores_up_to_date() {
        let mut s = signal("current");
        s.source = Some(SourceSignal {
            source_id: "/notes/a.md".to_string(),
            recorded_modified_at: days_ago(1),
            current_modified_at: Some(days_ago(10)), // 소스가 더 오래됨(변경 없음)
        });
        assert!(detect_external_mismatch(&[s]).is_empty(), "소스가 더 새롭지 않으면 불일치 아님");
    }

    #[test]
    fn test_mismatch_holds_when_current_unknown() {
        // 현재 소스 수정 시각을 확인 못 하면 판단 보류(오탐 방지).
        let mut s = signal("unknown source state");
        s.source = Some(SourceSignal {
            source_id: "/notes/a.md".to_string(),
            recorded_modified_at: days_ago(10),
            current_modified_at: None,
        });
        assert!(detect_external_mismatch(&[s]).is_empty(), "현재 시각 미상이면 보류");
    }

    #[test]
    fn test_mismatch_ignores_manual_docs() {
        // 출처 없는 수동 입력 문서는 외부 불일치 대상이 아니다.
        assert!(detect_external_mismatch(&[signal("manual")]).is_empty());
    }

    // ──── 탐지기 독립 실행·실패 격리 ────

    #[test]
    fn test_combine_isolates_failures() {
        // 한 탐지기가 실패해도 나머지 후보는 유지되어야 한다.
        let ok_a = ReviewCandidate {
            document_id: Uuid::new_v4(),
            kind: DetectorKind::Staleness,
            reason: "a".to_string(),
            evidence: json!({}),
        };
        let ok_b = ReviewCandidate {
            document_id: Uuid::new_v4(),
            kind: DetectorKind::Orphan,
            reason: "b".to_string(),
            evidence: json!({}),
        };
        let results = vec![
            (DetectorKind::Staleness, Ok(vec![ok_a.clone()])),
            (DetectorKind::Duplicate, Err(anyhow::anyhow!("mock 실패"))),
            (DetectorKind::Orphan, Ok(vec![ok_b.clone()])),
        ];
        let (candidates, failed) = combine_detector_results(results);
        assert_eq!(candidates.len(), 2, "실패한 탐지기만 빠지고 나머지는 유지");
        assert_eq!(failed, vec![DetectorKind::Duplicate], "실패 탐지기가 기록되어야 한다");
    }

    #[test]
    fn test_combine_all_ok() {
        let results = vec![
            (DetectorKind::Staleness, Ok(vec![])),
            (DetectorKind::Orphan, Ok(vec![])),
        ];
        let (candidates, failed) = combine_detector_results(results);
        assert!(candidates.is_empty());
        assert!(failed.is_empty());
    }
}
