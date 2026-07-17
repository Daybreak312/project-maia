# prd/ — 제품 요구사항 문서

작업의 **"무엇을·왜"**를 담는 PRD가 사는 곳이다.
"어떻게"는 [../exec-plans/](../exec-plans/README.md)가 담당한다.

## 언제 PRD를 쓰나

- 새 기능·구조 변경처럼 **설계 갈림길이 있는** 작업 — 착수 전에 작성하고
  소유자 승인을 받는다 (`CLAUDE.md`의 How vs What 경계).
- known-issues 항목 해소 같은 자명한 소규모 작업은 PRD 생략 가능 —
  ExecPlan만으로 진행 ([exec-plans/README.md](../exec-plans/README.md) 참조).

## 형태

- **단일 파일**: `docs/prd/{이름}.md` — 대부분의 작업.
- **폴더 (페이즈 분할)**: `docs/prd/{이름}/00-overview.md` + `01-phase1-*.md` + … +
  `review-self.md` — 여러 페이즈로 나눠 실행할 큰 작업.
  [prd-maia-brain/](../../prd-maia-brain/00-overview.md)(Phase 1~6)이 이 패턴의 선례다.
- 템플릿: [_templates/prd.md](../_templates/prd.md)

## 상태 관리

PRD 상단 `Status`를 현행으로 유지한다: Draft → 승인 대기 → 구현 중(exec-plan 링크)
→ 완료(커밋/PR 링크). 구현 완료 후 코드가 더 진화하면 PRD는 **갱신하지 않는다** —
PRD는 "그때의 의도"이고, 현재 사실은 docs/ 본문 문서가 답한다.

## review-self

페이즈 분할 작업이 끝나면 `review-self.md`로 자가 리뷰를 남긴다 — 무엇이 계획과
달랐고, 무엇을 다음에 다르게 할지. prd-maia-brain/review-self.md가 선례다.

## 기존 PRD

`prd-maia-brain/`(레포 루트)은 Phase 1~6의 역사 기록으로 **동결**돼 있다 —
갱신하지 않는다. 새 PRD는 전부 이 폴더(`docs/prd/`)에 작성한다.
