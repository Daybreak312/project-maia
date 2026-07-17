# ADR-001: MCP 서버를 TypeScript Thin Wrapper로 구현

- **Date**: 2026-03-02
- **Status**: Accepted

## Context

AI 도구들(Claude Code, OpenClaw 등)이 Maia를 쓰려면 MCP 표면이 필요하다.
Rust 백엔드에 MCP를 내장할지, 별도 프로세스로 둘지의 갈림길.

## Decision

Rust 네이티브 MCP 내장 대신, **별도 TypeScript MCP 서버가 Maia REST API를 HTTP로
호출하는 구조**를 채택한다.

## Rationale

- MCP TypeScript SDK가 가장 성숙하고 레퍼런스가 풍부하다.
- 기존 Rust 백엔드 코드 변경이 제로다.
- MCP 서버는 얇은 번역 레이어일 뿐, 비즈니스 로직이 없다.
- STDIO transport라 별도 포트가 불필요하다 (AI 도구가 프로세스를 직접 spawn).

- **기각한 대안:**
  - (a) Rust MCP SDK (rmcp) → 단일 바이너리 유지 가능하나 SDK 성숙도 부족.
  - (b) Spring Boot MCP Starter → JVM 추가, 기술 스택 이질감.

## Consequences

### 긍정적

- `mcp/`는 현재도 2파일(index.ts, maia-client.ts)짜리 얇은 번역 레이어로 유지되고
  있다 — 새 tool 추가 = REST를 먼저 만들고 번역 한 줄.

### 부정적/주의

- **MCP 서버에 로직을 넣기 시작하면 이 결정이 무너진다** — 로직은 항상 백엔드
  REST 뒤에 둔다 ([guardrails/README.md](../guardrails/README.md) 금지 패턴).

## 관련 문서

- [mcp.md](../mcp.md) — tool 6종 현황
- ADR-004 — REST가 유니버설 인터페이스 (이 결정의 일반화)

---
*원 기록: `contexts/decision_log.md` DEC-001 (2026-07-17 ADR로 이관).*
