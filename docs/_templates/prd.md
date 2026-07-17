# {기능명} — PRD

> **Version:** 1.0
> **Date:** {YYYY-MM-DD}
> **Status:** {Draft | 승인 대기 | 구현 중 (exec-plan 링크) | 완료 (커밋/PR)}

## Repository & Branching

| Item | Value |
|------|-------|
| **Base branch** | main |
| **Work branch** | {브랜치명 또는 "main 직행"} |
| **Code location** | {주 작업 경로} |

## Background (이 PRD가 존재하는 이유)

{어떤 맥락에서 이 작업이 요청됐는가. 문제의 역사.}

## Problem

1. {구체적 문제 — 번호 목록}

## Solution

{해법의 골자. 파이프라인/구조 변화는 텍스트 다이어그램으로.}

## 기능 요구사항 (FR)

1. {FR-1}

## 비기능 요구사항 · 불변식 (NFR)

- {지켜야 할 불변식 — [overview.md](../overview.md) 불변식과의 관계 명시}

## Key Decisions

{설계 갈림길과 선택. 굵직한 것은 ADR 후보로 표시 → 완료 시 [decisions/](../decisions/index.md)에 기록.}

## Anti-Goals (이번에 하지 않는 것)

- {의도적으로 범위에서 뺀 것과 이유}

## 엣지 케이스

- {실패 경로, 동시성, 빈 입력 등}

## 종료 조건

- [definition-of-green.md](../definition-of-green.md) 게이트 전부 통과
- {기능 고유의 검증 — 재현 시나리오, 수치 기준}

## 미해결 질문

- {소유자 판단이 필요한 항목 — 없으면 "없음"}

---

*페이즈 분할이 필요한 큰 작업은 단일 파일 대신 폴더로:
`docs/prd/{이름}/00-overview.md` + `01-phase1-*.md` + … + `review-self.md`
([prd-maia-brain/](../../prd-maia-brain/00-overview.md)이 선례).*
