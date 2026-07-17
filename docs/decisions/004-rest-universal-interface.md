# ADR-004: Maia REST API를 유니버설 인터페이스로

- **Date**: 2026-03-02
- **Status**: Accepted

## Context

MCP 외에도 GPT Actions 등 어댑터가 늘어날 수 있다. 어댑터마다 백엔드 내부에
접근하면 결합이 폭발한다.

## Decision

MCP, GPT Actions, 기타 미래의 어댑터 모두 **Maia REST API를 호출하는 구조**로
통일한다.

## Rationale

- Maia 백엔드가 Single Source of Truth.
- 어댑터는 프로토콜 번역만 담당한다.
- 새로운 AI 도구 연동 시 얇은 어댑터만 추가하면 되고, 백엔드 변경 없이 연동이
  확장된다.

## Consequences

### 긍정적

- 새 MCP tool의 표준 절차가 "REST 먼저, 번역 나중"으로 고정됐다
  ([development.md](../development.md) 표준 지점).

### 부정적/주의

- REST 계약 변경은 혼자 끝나지 않는다 — 모든 어댑터가 소비자다
  ([guardrails/api-change.md](../guardrails/api-change.md)).

## 관련 문서

- [api.md](../api.md) · [mcp.md](../mcp.md)

---
*원 기록: `contexts/decision_log.md` DEC-004 (2026-07-17 ADR로 이관).*
