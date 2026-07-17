# ADR-006: 검색 점수를 Raw Cosine Similarity로 표시

- **Date**: 2026-03-02
- **Status**: Accepted

## Context

RRF 정규화 점수(0.96, 0.93 등)는 상대적 순위이지 절대적 관련도가 아닌데, 사용자
화면에는 "모든 결과가 90%+"로 표시되어 오해를 유발했다.

## Decision

RRF 정규화 점수 대신 **벡터 검색의 raw cosine similarity**를 사용자에게 표시한다.
노이즈는 동적 필터링(절대 임계 0.5 + 점수 드롭 0.15 + 상한 5개)으로 제거한다.

## Rationale

- Raw cosine similarity는 정직한 절대적 유사도 지표다 — 순위 융합(RRF)은 내부
  랭킹용, 표시는 절대 지표용으로 역할을 분리한다.

## 관련 문서

- [search.md](../search.md) — RRF·필터링 현황

---
*원 기록: `contexts/decision_log.md` DEC-006 (2026-07-17 ADR로 이관).*
