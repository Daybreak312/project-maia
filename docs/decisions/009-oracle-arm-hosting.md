# ADR-009: Oracle Cloud ARM으로 호스팅 이전

- **Date**: 2026-07-08
- **Status**: Accepted

## Context

맥 로컬 호스팅은 로컬 임베딩·파싱(Phase 6)의 리소스 부담을 소유자 작업 머신에
지우고, 맥이 꺼지면 가용성이 끊긴다. 이전 대상 인스턴스에는 동거 서비스가 있다.

## Decision

스택 전체를 Oracle ARM 인스턴스로 이전한다.

- **CPU 캡**(cpu_shares 512) + **`127.0.0.1` 바인딩** + SSH 터널 접근.
- 이미지는 레지스트리 없이 save/load 이송 (`deploy/oracle/docker-compose.yml`).

## Rationale

- 로컬 임베딩·파싱의 맥 리소스 부담 제거, 상시 가용성 확보.
- 클라이언트 설정은 터널 덕에 무변경 — 이전 비용이 인프라 계층에 국한된다.

## Consequences

### 긍정적

- 상시 구동 + 일일 백업 체계가 이 위에서 가동 중이다.

### 부정적/주의

- 동거 서비스 보호가 상수 제약이 됐다 — 리소스 캡(cpu/mem)은 의도된 설계이며,
  성능 문제의 해법으로 캡 상향을 먼저 꺼내지 않는다
  ([guardrails/deploy-change.md](../guardrails/deploy-change.md)).

## 관련 문서

- [deployment.md](../deployment.md) — compose 3종 비교 · [operations.md](../operations.md) — 백업/복구

---
*원 기록: `contexts/decision_log.md` DEC-009 (2026-07-17 ADR로 이관).*
