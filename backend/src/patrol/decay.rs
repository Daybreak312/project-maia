//! 엣지 시간 감쇠 — Patrol이 자동 수행하는 **수학적 유지보수**(사람 판단 불필요).
//!
//! 오래된 엣지의 가중치를 지수 곡선으로 낮춰, 검색 그래프 확장에서 자연히 뒤로 밀리게
//! 한다. Phase 2의 시간 감쇠 lambda를 재사용한다(`exp(-lambda * age_days)`).
//!
//! **멱등성 불변식**: 각 엣지의 `base_weight`(생성 시점 원본 가중치)를 기준으로
//! `created_at` 나이만큼 **매번 재계산**하므로, 같은 시각에 몇 번을 돌려도 결과가 같다.
//! 최초 감쇠 때 현재 weight를 base로 고정한다(구버전 엣지 하위호환). 이 base가 없으면
//! 감쇠가 반복 실행마다 누적 곱해져 잘못 붕괴할 것이다 — base가 그것을 막는 핵심이다.

use chrono::{DateTime, Utc};

use crate::models::Document;

/// 엣지 하나의 감쇠된 가중치를 계산한다 (순수 함수).
///
/// `base`(생성 시점 원본 가중치)에 `exp(-lambda * age_days)`를 곱한다. 나이는 음수가
/// 되지 않도록 0으로 클램프하고(시계 뒤틀림 방어), 결과는 [0,1]로 클램프한다.
pub fn decayed_weight(
    base: f32,
    created_at: DateTime<Utc>,
    now: DateTime<Utc>,
    lambda: f32,
) -> f32 {
    let age_days = (now - created_at).num_seconds() as f64 / 86_400.0;
    let age_days = age_days.max(0.0);
    let factor = (-(lambda as f64) * age_days).exp();
    (base as f64 * factor).clamp(0.0, 1.0) as f32
}

/// 문서의 모든 엣지에 시간 감쇠를 적용한다(제자리 변경). 가중치가 바뀐 엣지 수를 반환.
///
/// 각 엣지의 `base_weight`가 없으면 현재 weight를 base로 고정하고, 있으면 그 base로부터
/// 재계산한다 → 반복 호출이 멱등이다(두 번째 호출은 변화 0). `lambda`가 0이면 감쇠 없음.
///
/// **불변식 보증**: weight가 실제로 바뀌는 엣지는 반드시 base_weight도 `Some`으로 고정되고,
/// 호출 측은 `changed > 0`일 때 문서를 저장한다 — 따라서 "weight != 원본"인 엣지는 항상
/// base가 영속화되어, 저장되지 않은 base 재계산이 원본과 어긋나는 일이 없다.
pub fn apply_edge_decay(doc: &mut Document, now: DateTime<Utc>, lambda: f32) -> usize {
    let mut changed = 0;
    for edge in &mut doc.edges {
        // base가 없으면(구버전/신규) 현재 weight를 원본으로 간주해 고정한다.
        let base = edge.base_weight.unwrap_or(edge.weight);
        edge.base_weight = Some(base);
        let new_weight = decayed_weight(base, edge.created_at, now, lambda);
        if (new_weight - edge.weight).abs() > f32::EPSILON {
            edge.weight = new_weight;
            changed += 1;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Document, Edge, RelationType};
    use uuid::Uuid;

    fn make_doc() -> Document {
        Document::new("raw".to_string(), "summary".to_string(), vec![], vec![])
    }

    fn days_ago(days: i64) -> DateTime<Utc> {
        Utc::now() - chrono::Duration::days(days)
    }

    // ──── decayed_weight (순수 곡선) ────

    #[test]
    fn test_decayed_weight_zero_age_unchanged() {
        let now = Utc::now();
        // 나이 0이면 factor=1 → base 그대로.
        assert!((decayed_weight(0.8, now, now, 0.01) - 0.8).abs() < 1e-6);
    }

    #[test]
    fn test_decayed_weight_lambda_zero_no_decay() {
        let now = Utc::now();
        // lambda=0이면 아무리 오래돼도 감쇠 없음.
        assert!((decayed_weight(0.8, days_ago(1000), now, 0.0) - 0.8).abs() < 1e-6);
    }

    #[test]
    fn test_decayed_weight_older_is_smaller() {
        let now = Utc::now();
        let young = decayed_weight(1.0, days_ago(10), now, 0.05);
        let old = decayed_weight(1.0, days_ago(100), now, 0.05);
        assert!(old < young, "더 오래된 엣지가 더 많이 감쇠");
        assert!(young < 1.0, "감쇠가 적용되어야 한다");
        assert!(old > 0.0);
    }

    #[test]
    fn test_decayed_weight_known_value() {
        // exp(-0.1 * 10) = exp(-1) ≈ 0.3679. base 1.0 → ≈0.3679.
        let now = Utc::now();
        let w = decayed_weight(1.0, days_ago(10), now, 0.1);
        assert!((w - 0.367_879_44).abs() < 1e-3, "예상 감쇠 값과 근접해야 한다: {w}");
    }

    #[test]
    fn test_decayed_weight_future_created_clamped_to_zero_age() {
        // created_at이 미래(시계 뒤틀림)여도 음수 나이가 되지 않아 base를 넘지 않는다.
        let now = Utc::now();
        let future = now + chrono::Duration::days(5);
        assert!((decayed_weight(0.6, future, now, 0.1) - 0.6).abs() < 1e-6);
    }

    #[test]
    fn test_decayed_weight_result_clamped() {
        let now = Utc::now();
        // base가 범위를 벗어나도 결과는 [0,1]. (base>1 방어)
        assert!(decayed_weight(2.0, now, now, 0.0) <= 1.0);
    }

    // ──── apply_edge_decay (문서 제자리 감쇠) ────

    #[test]
    fn test_apply_decay_lowers_old_edge_and_sets_base() {
        let mut doc = make_doc();
        let mut e = Edge::new(Uuid::new_v4(), RelationType::RelatedTo, 0.9);
        e.created_at = days_ago(100);
        doc.edges.push(e);

        let changed = apply_edge_decay(&mut doc, Utc::now(), 0.05);
        assert_eq!(changed, 1, "오래된 엣지 1개가 감쇠");
        assert!(doc.edges[0].weight < 0.9, "가중치가 낮아져야 한다");
        assert_eq!(doc.edges[0].base_weight, Some(0.9), "base가 원본으로 고정되어야 한다");
    }

    #[test]
    fn test_apply_decay_idempotent() {
        // 두 번째 실행은 아무것도 바꾸지 않아야 한다(멱등) — base 기준 재계산이므로.
        let mut doc = make_doc();
        let mut e = Edge::new(Uuid::new_v4(), RelationType::RelatedTo, 0.9);
        e.created_at = days_ago(100);
        doc.edges.push(e);

        let now = Utc::now();
        let first = apply_edge_decay(&mut doc, now, 0.05);
        let weight_after_first = doc.edges[0].weight;
        let second = apply_edge_decay(&mut doc, now, 0.05);

        assert_eq!(first, 1);
        assert_eq!(second, 0, "같은 시각 재실행은 변화 0(멱등)");
        assert!((doc.edges[0].weight - weight_after_first).abs() < f32::EPSILON);
        assert_eq!(doc.edges[0].base_weight, Some(0.9), "base는 원본 유지");
    }

    #[test]
    fn test_apply_decay_base_preserved_across_reruns_with_advancing_time() {
        // 시간이 흘러 재실행해도 base는 원본을 유지하고, 감쇠는 원본으로부터 재계산된다
        // (누적 곱이 아니라 절대 나이 기준 — 곡선 위의 점을 정확히 되짚는다).
        let mut doc = make_doc();
        let created = days_ago(200);
        let mut e = Edge::new(Uuid::new_v4(), RelationType::RelatedTo, 1.0);
        e.created_at = created;
        doc.edges.push(e);

        // 100일 시점 감쇠
        apply_edge_decay(&mut doc, created + chrono::Duration::days(100), 0.02);
        assert_eq!(doc.edges[0].base_weight, Some(1.0));
        // 200일 시점 재감쇠 — base는 여전히 1.0, 결과는 곡선 값과 일치해야 한다.
        apply_edge_decay(&mut doc, created + chrono::Duration::days(200), 0.02);
        let expected = decayed_weight(1.0, created, created + chrono::Duration::days(200), 0.02);
        assert!((doc.edges[0].weight - expected).abs() < 1e-6, "절대 나이 기준 곡선 값과 일치");
    }

    #[test]
    fn test_apply_decay_lambda_zero_no_change() {
        let mut doc = make_doc();
        let mut e = Edge::new(Uuid::new_v4(), RelationType::RelatedTo, 0.7);
        e.created_at = days_ago(500);
        doc.edges.push(e);

        let changed = apply_edge_decay(&mut doc, Utc::now(), 0.0);
        assert_eq!(changed, 0, "lambda=0이면 변화 없음");
        assert!((doc.edges[0].weight - 0.7).abs() < f32::EPSILON);
    }

    #[test]
    fn test_apply_decay_fresh_edge_negligible() {
        // 갓 만든 엣지는 사실상 변화 없음(나이≈0).
        let mut doc = make_doc();
        doc.edges.push(Edge::new(Uuid::new_v4(), RelationType::RelatedTo, 0.8));
        let changed = apply_edge_decay(&mut doc, Utc::now(), 0.05);
        assert_eq!(changed, 0, "나이 0 엣지는 변화 없음");
    }

    #[test]
    fn test_apply_decay_no_edges_returns_zero() {
        let mut doc = make_doc();
        assert_eq!(apply_edge_decay(&mut doc, Utc::now(), 0.05), 0);
    }

    #[test]
    fn test_apply_decay_counts_multiple_changed() {
        let mut doc = make_doc();
        for _ in 0..3 {
            let mut e = Edge::new(Uuid::new_v4(), RelationType::RelatedTo, 0.9);
            e.created_at = days_ago(80);
            doc.edges.push(e);
        }
        assert_eq!(apply_edge_decay(&mut doc, Utc::now(), 0.05), 3);
    }
}
