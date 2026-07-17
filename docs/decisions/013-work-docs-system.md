# ADR-013: 작업 문서 체계 도입 — ADR·guardrails·prd/exec-plans·templates·green

- **Date**: 2026-07-17
- **Status**: Accepted

## Context

ADR-012로 사실 레퍼런스(docs/)는 정리됐지만, **작업 단위 문서**에는 구조가 없었다:

- 결정 이력이 `contexts/decision_log.md` 단일 파일에 2~3줄 요약으로만 쌓여,
  결정의 배경·기각 대안·결과를 담을 공간이 없었다.
- "이 유형의 작업에서 무엇을 조심해야 하는가"(운영 위험·불변 규칙)가 여러 문서에
  흩어져 있어, 작업 착수 시점에 체크리스트로 걸리지 않았다.
- 새 작업의 스펙·실행 계획이 `contexts/spec.md` 한 파일을 덮어쓰는 구조라,
  동시 작업·이력 추적·계획 간 의존 표현이 불가능했다.
- 검증 게이트 명령이 development.md·spec.md·policy.md에 중복 서술돼 있었다.

참고 모델: **VacatioInc/vacatio-platform**의 docs 시스템 — 100% AI 코딩 레포에서
"docs/가 세션 간 문맥을 유지하는 유일한 안전장치"라는 전제로 ADR 50편·guardrails·
prd/exec-plans(active/completed)·_templates·DEFINITION-OF-GREEN을 운용한다.
Maia도 사실상 전량 AI가 작성하는 레포라 같은 전제가 성립한다. `prd-maia-brain/`은
이미 vacatio의 PRD 폴더 패턴(00-overview + phase 분할 + review-self)과 동일했다 —
이번 도입은 그 방법론의 나머지 절반이다.

## Decision

`docs/` 아래에 작업 문서 체계를 신설한다:

1. **`decisions/`** — ADR 개별 파일(NNN-슬러그.md) + index.md.
   decision_log.md의 DEC-001~012를 ADR-001~012로 이관(원문 보존·구조 재배치),
   `contexts/decision_log.md`는 스텁화. 새 결정은 ADR로만 기록한다.
2. **`guardrails/`** — 작업 유형별 체크리스트 6종(api/schema/llm-provider/connector/
   deploy/bugfix) + 공통 금지 패턴 표(README).
3. **`prd/`** + **`exec-plans/`**(active/, completed/) — "무엇을·왜"와 "어떻게·진행"의
   분리. exec-plan frontmatter에 `prd`/`depends_on`/`supersedes`. 완료 시 active →
   completed로 `git mv`. 새 PRD는 docs/prd/에, `prd-maia-brain/`은 동결 유지.
4. **`_templates/`** — adr/guardrail/prd/exec-plan 4종.
5. **`definition-of-green.md`** — 검증 게이트의 단일 정의. 다른 문서는 참조만.

## Rationale

- **왜 ADR 개별 파일인가:** 단일 로그 파일은 항목당 공간이 압축을 강제한다 —
  기각 대안·Consequences가 빠지면 "왜 이렇게 안 했지?"를 매번 다시 판단하게 된다.
  개별 파일 + Status 필드(Superseded 표기)는 결정의 생명주기를 추적 가능하게 한다.
- **왜 guardrails인가:** policy.md는 규칙의 SSoT지만 "법전"이라 작업 시점에 전부
  다시 읽게 되지 않는다. 작업 유형별 진입점으로 재편성하면 해당 작업에 걸리는
  규칙·함정(known-issues)만 체크리스트로 걸린다. 규칙 원문은 여전히 policy.md —
  guardrail은 그 실행 뷰다.
- **왜 prd/exec-plans 분리인가:** spec.md 덮어쓰기 구조는 "지금 하나의 작업"만
  표현한다. 파일 단위 + 라이프사이클 폴더는 대기·진행·완료가 파일 시스템에
  드러나고, supersedes로 계획 간 대체 관계를 표현한다.
- **vacatio에서 이식하지 않은 것 (규모 판단):**
  - (a) `modules/` 바운디드 컨텍스트 카드 → Maia는 docs/ 주제별 문서 8편(search·
    ingest·patrol 등)이 이미 그 역할이다. 이중화하면 불일치만 생긴다.
  - (b) `backend/docs`·`frontend/docs` 스코프 분산 → 단일 시스템 + 문서 ~30편
    규모에서는 단일 docs/가 탐색 비용이 더 낮다. 컴포넌트별 문서가 10편을 넘게
    되면 재검토.
  - (c) `loops/` → Maia에는 자율 실행 루프가 없다. DEFINITION-OF-GREEN 개념만
    docs/definition-of-green.md로 흡수했다.

## Consequences

### 긍정적

- 작업 착수의 표준 경로가 생겼다: PRD(무엇) → ExecPlan(어떻게) → guardrail(조심) →
  green(완료 기준) → ADR(결정 기록).
- 결정·계획·위험이 각자 살 곳을 가져, 한 파일이 비대해지며 썩는 ADR-012 이전
  패턴의 재발을 막는다.

### 부정적/주의

- 문서 종류가 늘어난 만큼 **라우팅 규칙이 무너지면 혼란도 커진다** —
  "어디에 쓰는가"는 docs/README.md 배치 표가 단일 기준이다.
- 빈 active/는 정상 상태다(운영 단계) — 채우기 위해 작업을 만들지 않는다.
  착수는 여전히 소유자 승인 후다 (How vs What 경계).

## 관련 문서

- [docs/README.md](../README.md) — 전체 배치 원칙
- ADR-012 — 1차 개편 (사실 레퍼런스 분리)

---
*이 ADR부터 네이티브 작성이다 (이관 아님). 템플릿: [_templates/adr.md](../_templates/adr.md).*
