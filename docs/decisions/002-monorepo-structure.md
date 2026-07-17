# ADR-002: 모노레포 구조 전환 (backend/ + mcp/ + frontend/)

- **Date**: 2026-03-02
- **Status**: Accepted

## Context

MCP 서버(ADR-001)와 프론트엔드가 생기며 루트에 있던 백엔드 코드와 빌드 단위가
섞이기 시작했다.

## Decision

루트에 있던 백엔드 코드를 `backend/`로, 프론트엔드를 `frontend/`로 분리하고
`mcp/`를 추가한다. 이동은 `git mv`로 히스토리를 보존한다.

## Rationale

- MCP, Frontend, Backend가 독립적인 빌드 단위다 (cargo / npm / npm).
- 각 모듈의 관심사가 명확히 분리된다.

## Consequences

### 긍정적

- 컴포넌트별 검증 게이트가 자연스럽게 분리된다 —
  [definition-of-green.md](../definition-of-green.md)의 3게이트 구조가 이 위에 선다.

## 관련 문서

- [architecture.md](../architecture.md) — 모노레포 구조 현황

---
*원 기록: `contexts/decision_log.md` DEC-002 (2026-07-17 ADR로 이관).*
