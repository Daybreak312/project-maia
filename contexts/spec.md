# Current Task — 활성 작업 포인터

> 이 파일은 "지금 무엇이 진행 중인가"의 **포인터**만 담는다. 작업의 실체는
> [`docs/prd/`](../docs/prd/README.md)("무엇을")와
> [`docs/exec-plans/active/`](../docs/exec-plans/README.md)("어떻게")에 산다 (ADR-013).
> 활성 작업이 없으면 비어 있는 것이 정상이다.

## 활성 작업

**없음 — 운영 단계.** (2026-07-17 기준)

`docs/exec-plans/active/`도 비어 있다. 직전 완료: Phase 6 + sonnet-5 핫픽스(712484e)
+ 문서 체계 개편 1·2차(ce73304, ADR-012·013).

## 다음 작업 후보 (착수 시 PRD/ExecPlan으로 승격)

우선순위는 [docs/known-issues.md](../docs/known-issues.md) 권고 순서를 따른다:

1. **H1** — `MAIA_API_KEY` fail-open 방어 (compose 필수 치환 → 앱 레벨 fail-closed)
2. **H2** — settings.json 손상 시 백업+명시 에러 (auth/keys.rs 패턴 이식)
3. **M5** — qdrant 이미지 태그 고정
4. **M1·M2·M4** — 프로바이더 견고화 묶음 (재시도/3상 검증/JSON 추출/강등 관측)
5. **M3·M6** — poison item dead-letter 정책 + thinking 제어 (설계 판단 필요)

착수는 소유자 승인 후 (`policy.md`의 How vs What 경계). 착수 절차:

1. 갈림길 있는 작업이면 `docs/prd/`에 PRD 작성 → 승인.
2. `docs/exec-plans/active/`에 ExecPlan 작성
   (템플릿: [docs/_templates/exec-plan.md](../docs/_templates/exec-plan.md)).
3. 이 파일의 "활성 작업"에 그 링크를 건다.
