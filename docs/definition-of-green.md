# Definition of Green — 검증 게이트 SSoT

> 최종 검증: 2026-07-17 · 기준 커밋: ce73304 · [문서 인덱스](README.md)

"작업 완료"를 선언하기 전에 통과해야 하는 게이트의 **단일 정의**다.
다른 문서(development.md, exec-plan, guardrail)는 이 파일을 **참조만** 한다 —
게이트 명령을 문서마다 재작성하지 않는다. 빌드/테스트 명령이 바뀌면
그 변경 커밋에서 이 파일을 함께 갱신한다.

## 게이트

건드린 컴포넌트의 게이트만 실행하면 된다 (backend만 수정 → backend 게이트만).

| 컴포넌트 | 명령 | 기대 |
|----------|------|------|
| backend | `cd backend && cargo test` | exit 0, failed 0 (Phase 6 기준 544개) |
| frontend | `cd frontend && npm run build` | exit 0 (`tsc -b` + vite build) |
| mcp | `cd mcp && npm run build` | exit 0 (`tsc`) |

경계를 넘는 변경은 양쪽 게이트를 함께 돌린다:

- REST API 계약 변경 → backend + mcp (+ frontend가 그 API를 쓰면 frontend도)
- raw JSON 스키마 변경 → backend (하위호환 로드 테스트 포함 — [guardrails/schema-change.md](guardrails/schema-change.md))

## 게이트 외 규칙

- **테스트 격리**: 단위 테스트는 Qdrant·실 LLM 무의존 — mock provider,
  trait 주입(`SearchBackend`/`PatrolExecutor`)으로 전 분기를 고정한다.
- **회귀 동반**: 버그 수정에는 재발 방지 회귀 테스트를 동반한다
  (`policy.md` `[Testing] - [Gate]`).
- **문서 게이트**: 코드 동작·구조가 바뀌었으면 [docs/README.md](README.md)
  매핑표 기준으로 문서를 같은 커밋에서 갱신한다.

## 보고 규칙

결과 보고는 **정확한 수치와 exit code**로 한다 — "544 passed, 0 failed, exit 0".
"잘 됩니다 / 정상 동작합니다" 류의 판정 문구만으로 게이트 통과를 주장하지 않는다.
