# ADR-008: Phase 6 — 구독 프로바이더 + 로컬 임베딩으로 종량제 탈피

- **Date**: 2026-07-07
- **Status**: Accepted (부분 개정: 파싱 기본 모델은 ADR-010이 haiku → sonnet-5로 갱신)

## Context

파싱·임베딩이 종량제 API 키(Gemini/OpenAI)에 의존하면, 키 잔액·정책 변경이
개인 기억 시스템의 가용성을 좌우한다. 소유자는 이미 Claude·ChatGPT 구독을
지불하고 있다.

## Decision

1. 파싱: **Claude 구독 OAuth**(`sk-ant-oat` 자동 감지)와 **ChatGPT Codex**(auth.json
   임포트)를 추가한다.
2. 임베딩: **로컬 fastembed**(multilingual-e5-small, 384d)를 추가한다.
3. 기존 Gemini/OpenAI provider는 유지한 채 능력만 확장한다.
4. LLM HTTP 타임아웃 60→300초 (thinking 모델의 대형 문서 파싱 실측).

## Rationale

종량제 키 의존 제거 — 소유자가 이미 지불 중인 구독과 로컬 연산으로 자립한다
(`primary.md` Core Value "자립").

## Consequences

### 긍정적

- 종량제 키 제로 운영이 실현됐다 (2026-07-17부터 실 배포 적용).

### 부정적/주의

- 비공식 업스트림(Codex 버전 게이팅)은 드리프트 리스크 — `llm/codex.rs`
  `upstream` 모듈 단일 지점 규칙(`policy.md` `[Backend] - [Upstream]`)으로 관리.
- 로컬 임베딩은 메모리를 상주 소비 — 프로세스 전역 모델 캐시로 중복 로드 방지.

## 관련 문서

- [llm-providers.md](../llm-providers.md) — provider 5종·차원 표
- [guardrails/llm-provider-change.md](../guardrails/llm-provider-change.md)

---
*원 기록: `contexts/decision_log.md` DEC-008 (2026-07-17 ADR로 이관).*
