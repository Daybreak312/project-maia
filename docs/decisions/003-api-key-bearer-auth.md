# ADR-003: API 키 인증 (Bearer Token)

- **Date**: 2026-03-02
- **Status**: Accepted

## Context

EC2/온프레미스 배포로 외부 노출이 전제라 인증이 필수다. 개인용 단일 사용자
시스템이라 계정 체계는 과잉이다.

## Decision

`MAIA_API_KEY` 환경변수 기반 Bearer 토큰 인증을 axum 미들웨어로 구현한다.
미설정 시 인증 비활성(개발 편의), `/health`만 인증 제외(모니터링).

## Rationale

- 개인용 시스템이므로 단일 정적 API 키로 충분하다.
- 미설정 시 비활성화로 로컬 개발 편의를 유지한다.

## Consequences

### 긍정적

- 이후 워크스페이스별 발급 키·admin 경계가 이 축 위에 확장됐다 ([api.md](../api.md)).

### 부정적/주의

- **"미설정 시 비활성"이 운영에서는 fail-open 리스크가 됐다** — 키 유실·치환 실패
  1회가 곧 전면 공개 ([known-issues.md](../known-issues.md) H1). 완화는 compose 필수
  치환, 근본 해결(기동 거부 + dev opt-in 역전)은 백로그.

## 관련 문서

- [api.md](../api.md) — 인증·권한 모델
- [guardrails/deploy-change.md](../guardrails/deploy-change.md) — 배포 시 방어선

---
*원 기록: `contexts/decision_log.md` DEC-003 (2026-07-17 ADR로 이관).*
