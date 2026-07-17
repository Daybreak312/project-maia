# ADR-011: 레포 공개 전환

- **Date**: 2026-07-17
- **Status**: Accepted

## Context

소유자 결정으로 레포를 공개한다. 공개 전 검증이 필요했다.

## Decision

https://github.com/Daybreak312/project-maia 를 공개한다 (라이선스 미설정, 전체
히스토리 포함). 공개 전 전 히스토리 시크릿 스캔 0건을 확인했다.

## Consequences

### 긍정적

- 문서·코드가 외부 참조 가능해졌다.

### 부정적/주의

- **이후 모든 커밋은 공개를 전제로 한다** — 실 크리덴셜, 개인 배포 비밀(토큰·계정
  식별자·실 도메인 정보) 커밋 금지 (`policy.md` `[Security] - [Repo]`). 문서에도
  같은 규칙이 적용된다 ([docs/README.md](../README.md) 작성 원칙).

---
*원 기록: `contexts/decision_log.md` DEC-011 (2026-07-17 ADR로 이관).*
