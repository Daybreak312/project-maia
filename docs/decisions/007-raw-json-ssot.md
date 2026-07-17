# ADR-007: raw JSON = SSoT, 신규 DB 금지, 무조건 raw 폴백

- **Date**: 2026-07-06 (prd-maia-brain 킥오프)
- **Status**: Accepted

## Context

Phase 2(지식 그래프) 킥오프 시점 — 그래프 엣지를 어디에 저장할지가 갈림길이었다.
그래프 DB 도입은 자연스러워 보이는 선택지였다.

## Decision

1. 그래프 엣지를 포함한 **모든 진실을 raw JSON 파일**에 둔다.
2. Qdrant는 reindex로 전량 재생성 가능한 **파생 인덱스**로 유지한다.
3. **신규 DB 도입 금지.**
4. LLM·임베딩이 실패해도 **raw 저장은 반드시 성공**한다 (정보 유실 0).

## Rationale

개인 기억 시스템에서 최우선 가치는 "잃지 않는 것"과 "들고 떠날 수 있는 것"이다.
DB 추가는 운영 부담·이식성 저하 대비 이득이 없다.

## Consequences

### 긍정적

- 백업 = raw 디렉터리 복사, 복구 = 복사 + `POST /api/reindex`로 완결
  ([operations.md](../operations.md)).
- 이 결정이 시스템 전체 불변식의 뿌리가 됐다 —
  `policy.md` `[Data]` 절 전체가 이 ADR의 전개다.

### 부정적/주의

- 모든 신기능은 "raw에 먼저, 파생은 나중" 순서를 강제받는다 —
  위반 패턴은 [guardrails/README.md](../guardrails/README.md) 참조.

## 관련 문서

- [overview.md](../overview.md) — 불변식 · [data-model.md](../data-model.md) — 저장 구조

---
*원 기록: `contexts/decision_log.md` DEC-007 (2026-07-17 ADR로 이관).*
