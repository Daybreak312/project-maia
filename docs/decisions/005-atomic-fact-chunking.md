# ADR-005: Atomic Fact 청킹으로 검색 정밀도 향상

- **Date**: 2026-03-02
- **Status**: Accepted

## Context

1 Document = 1 Vector(summary) 구조에서는 긴 문서의 summary가 모든 세부 정보를
대표하지 못해 검색 누락이 발생했다.

## Decision

1 Document = **N Vectors(summary + facts)** 구조로 전환한다. LLM이 문서를 독립적
사실 문장으로 분해하고, 각 fact에 개별 임베딩을 부여한다.

구현 핵심:

- Qdrant Point ID는 chunk별 랜덤 UUID, `document_id` payload 필드로 문서 소속 식별.
- 삭제는 `document_id` 필터 기반 (1개 문서 = N개 point 일괄 삭제).
- 검색은 chunk 단위 유사도 → document 그룹핑 → matched_facts 수집.

## Rationale

- 어떤 각도의 질문이든 해당 사실의 벡터에 직접 매칭 가능해진다.
- `#[serde(default)]`로 기존 문서 하위호환, reindex로 점진 마이그레이션 —
  스키마 하위호환 원칙(→ ADR-007, `policy.md` `[Data] - [Compat]`)의 선례.

## 관련 문서

- [data-model.md](../data-model.md) — 청킹 구조 현황
- [search.md](../search.md) — chunk → document 그룹핑 검색 흐름

---
*원 기록: `contexts/decision_log.md` DEC-005 (2026-07-17 ADR로 이관).*
