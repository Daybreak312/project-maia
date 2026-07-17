# exec-plans/ — 실행 계획

작업의 **"어떻게"**를 담는 실행 계획(ExecPlan)이 사는 곳이다.
"무엇을·왜"는 [../prd/](../prd/README.md)가 담당한다 — ExecPlan은 PRD의 실행 도출본이다.

## 라이프사이클

```
작성 → active/{이름}.md → 실행 (마일스톤 단위 커밋) → 완료 → completed/{이름}.md (git mv)
```

- **`active/`** — 지금 실행 중이거나 착수 대기인 플랜. 여기 있는 파일 수가 곧
  "진행 중인 작업 수"다. 비어 있으면 운영 단계라는 뜻이다.
- **`completed/`** — 끝난 플랜. 삭제하지 않고 이동한다 — "그때 어떤 순서로 왜
  그렇게 했나"의 역사 기록이며, 후속 플랜이 `supersedes`로 참조한다.
- 완료 이동 시 함께 할 일: 결과 사실을 docs/에 반영(매핑표), 굵직한 결정은
  [../decisions/](../decisions/index.md)에 ADR로, [contexts/plan.md](../../contexts/plan.md)
  현황판 갱신.

## Frontmatter

```yaml
---
prd: docs/prd/foo.md   # 근거 PRD (소규모 작업은 null 허용)
depends_on: []          # 선행 완료가 필요한 다른 플랜
supersedes: []          # 이 플랜이 대체하는 완료 플랜 (경로)
---
```

`supersedes`로 지목된 완료 플랜은 "이후 다른 플랜이 이 결과를 일부 대체했다"는
표시다 — 읽는 쪽이 최신 상태를 오인하지 않게 한다.

## 작성 규칙

- 템플릿: [_templates/exec-plan.md](../_templates/exec-plan.md)
- 필수 골격: 참조(단일 출처 명시) → 베이스라인 → 검증 게이트
  ([definition-of-green.md](../definition-of-green.md) 참조) → 마일스톤(각각 목표·작업·
  검증·커밋 메시지) → 잔존 리스크.
- **한 마일스톤 = 하나의 완결된 커밋.** 각 마일스톤은 커밋 전에 게이트를 통과한다.
- 플랜은 "레포 현재 상태 + PRD"와 함께 단일 출처다 — 플랜이 낡았으면 실행 전에
  레포 실물을 확인하고 플랜을 먼저 고친다.

## PRD 없이 ExecPlan만 쓰는 경우

known-issues 항목 해소, 소규모 견고화처럼 "무엇을"이 이미 자명한 작업은 PRD 생략
가능 — 단 frontmatter `prd: null`과 함께 참조 섹션에 근거(known-issues 항목 등)를
명시한다.
