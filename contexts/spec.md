# Current Task Specs — 현재 진행 중인 작업

> 이 파일은 "지금 무엇을 만들고 있는가"만 담는다. 작업이 없으면 비어 있는 것이
> 정상이다. 완료된 스펙은 여기서 지우고, 결과 사실은 `docs/`에 반영한다.
> (역사적 스펙: Phase별 PRD → `prd-maia-brain/`, MVP 시절 스펙 → git 히스토리)

## 활성 스펙

**없음 — 운영 단계.** (2026-07-17 기준)

직전 완료: Phase 6(구독 프로바이더·로컬 임베딩) + sonnet-5 전환 핫픽스(커밋 712484e)
+ 문서 체계 개편(docs/ 신설).

## 다음 작업 후보 (착수 시 이 섹션을 스펙으로 승격)

우선순위는 [docs/known-issues.md](../docs/known-issues.md) 권고 순서를 따른다:

1. **H1** — `MAIA_API_KEY` fail-open 방어 (compose 필수 치환 → 앱 레벨 fail-closed)
2. **H2** — settings.json 손상 시 백업+명시 에러 (auth/keys.rs 패턴 이식)
3. **M5** — qdrant 이미지 태그 고정
4. **M1·M2·M4** — 프로바이더 견고화 묶음 (재시도/3상 검증/JSON 추출/강등 관측)
5. **M3·M6** — poison item dead-letter 정책 + thinking 제어 (설계 판단 필요)

착수는 소유자 승인 후 (`policy.md`의 How vs What 경계).

## 스펙 작성 규칙

새 작업 착수 시 이 파일에 다음 구조로 적는다:
목표 / 사용자 스토리 / 기능 요구사항(FR) / 비기능 요구사항·불변식 / 엣지 케이스 /
종료 조건(테스트 게이트). Phase 6 스펙(`prd-maia-brain/06-phase6-providers.md`)이
좋은 선례다.
