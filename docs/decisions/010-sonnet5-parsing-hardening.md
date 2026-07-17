# ADR-010: 파싱 모델 sonnet-5 전환 + 응답 견고화

- **Date**: 2026-07-17 (커밋 712484e)
- **Status**: Accepted

## Context

운영 사고 2건이 실측됐다:

1. 출력 상한 1024에서 대형 문서 JSON이 절단되어 "파싱 실패"로 원인이 은폐됐다.
2. sonnet-5의 adaptive thinking 블록이 경직된 역직렬화(ContentBlock 단일 구조체)를
   붕괴시켰다.

## Decision

1. Claude 파싱 모델 `claude-haiku-4-5` → **`claude-sonnet-5`** (ADR-008의 기본 모델
   부분 개정).
2. `max_tokens` 16384/8192 상향.
3. `stop_reason=max_tokens` **절단 가드** — 절단을 침묵 실패가 아닌 명시적 에러로.
4. ContentBlock을 **tagged enum**으로 — thinking 블록 관용 수용.

## Rationale

"침묵 실패 금지" 원칙(`policy.md` `[Backend] - [Failure]`)의 직접 적용이다 —
두 사고 모두 실패 자체보다 **원인 은폐**가 비용이었다.

## Consequences

### 긍정적

- 대형 문서 파싱이 안정화되고, 실패 시 원인이 에러 메시지에 드러난다.

### 부정적/주의

- thinking 토큰이 `max_tokens` 예산을 잠식한다 — 초대형 문서는 여전히 절단될 수
  있고, 그 문서는 poison item이 될 수 있다 ([known-issues.md](../known-issues.md)
  M6·M3). 파싱 호출의 thinking 제어는 백로그.

## 관련 문서

- [llm-providers.md](../llm-providers.md) · [guardrails/llm-provider-change.md](../guardrails/llm-provider-change.md)

---
*원 기록: `contexts/decision_log.md` DEC-010 (2026-07-17 ADR로 이관).*
