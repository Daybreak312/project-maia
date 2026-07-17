# ADR-012: 문서 체계 개편 — docs/ 신설

- **Date**: 2026-07-17 (커밋 ce73304)
- **Status**: Accepted

## Context

단일 `contexts/architecture.md`(537줄)가 레퍼런스·작업 컨텍스트·역사를 겸하며
비대해져 스테일 항목(모델명·모듈 트리)이 발생했다. `contexts/spec.md`·`plan.md`는
Phase 1 MVP 시절 정보로 멈춰 있었다.

## Decision

1. 사실 레퍼런스를 **`docs/`**(인덱스: docs/README.md)로 분리·신설한다 (본문 13편).
2. `contexts/architecture.md`는 스텁으로 이관한다.
3. `contexts/`는 **작업 컨텍스트**(정체성·정책·현재 스펙·현황판·결정 로그) 전용으로
   재정의한다.
4. `prd-maia-brain/`은 역사 기록으로 동결한다.
5. 문서마다 **"최종 검증일 + 기준 커밋" 배지**, **코드→문서 매핑표**로 동시 갱신
   규약을 수립한다.

## Rationale

참조 단위 분리 + 갱신 규약이 문서 부패를 막는 구조적 해법이다 — "어디를 고치면
어느 문서를 갱신하는가"가 표로 고정되면, 갱신 누락이 리뷰에서 걸린다.

## Consequences

### 긍정적

- "지금 시스템이 어떻게 동작하는가"의 단일 진입점(docs/README.md)이 생겼다.

### 부정적/주의

- 규약은 지켜질 때만 가치가 있다 — 배지 30일 초과 시 독자가 먼저 의심하고
  대조하는 규칙까지가 한 세트다.

## 관련 문서

- [docs/README.md](../README.md) — 인덱스·매핑표·배지 규약
- ADR-013 — 작업 문서 체계(prd/exec-plans/guardrails/ADR) 도입 (이 개편의 2차 확장)

---
*원 기록: `contexts/decision_log.md` DEC-012 (2026-07-17 ADR로 이관).*
