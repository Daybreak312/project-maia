# Patrol — 자기 관리와 메모리 거버넌스

> 최종 검증: 2026-07-17 · 기준 커밋: 712484e · [문서 인덱스](README.md)

## 철학

두뇌가 스스로의 기억 상태를 점검하는 **반자율** 계층이다. 완전 자동 수정의
자기참조 순환과 오탐 피로를 피하기 위해:

- 시스템은 **후보를 식별해 플래그만 세운다** (Review Queue).
- 판단(유효/수정 필요/삭제/기각)은 소유자가 한다.
- **Patrol 자체는 읽기 + 플래그 + 엣지 감쇠 재계산만** 하며 문서 내용을 변경·삭제하지
  않는다. 삭제는 오직 사람의 judge에서만 일어난다.

## 실행 흐름 (`backend/src/patrol/`)

```
스케줄러(주기) ─┐
               ├─▶ Patrol.run(ws) ─▶ 신호 수집(문서·피드백·freshness·소스 mtime)
API 트리거(수동)┘        ▼
                  탐지기 4종 (순수 함수, 실패 상호 격리)
                        ▼
                  Review Queue enqueue (열린 항목 dedup + 유형별 상한 50)
                        ├─▶ 엣지 시간 감쇠 자동 재계산
                        ├─▶ 메트릭 일 롤업
                        └─▶ 실행 이력 기록
```

## 탐지기 4종 (`patrol/detectors.rs`)

전부 **LLM 없이 수치 신호 기반**의 순수 함수다. 임계값은 워크스페이스
`patrol.strictness`에서 파생되며 기본은 보수적(큐 쓰레기장 방지).

| 유형 | 신호 |
|------|------|
| `staleness` | freshness 기준점 이후 경과 시간 + "관련 없음" 피드백 가중 |
| `duplicate` | 요약 토큰 Jaccard 유사도 상위 쌍 (동일 내용 재유입이 지배 사례) |
| `orphan` | 엣지 없는 문서 (신규 문서는 유예) |
| `external_mismatch` | 커넥터 소스 파일 수정 시각 > 문서 유입 시각 |

## 엣지 시간 감쇠 (`patrol/decay.rs`)

- `exp(-λ · age_days)`로 오래된 엣지 가중치를 낮춰 그래프 확장에서 뒤로 밀리게 한다.
- `Edge.base_weight`(생성 시점 원본) 기준으로 매번 재계산 → **반복 실행이 멱등**.
  base가 없는 구버전 엣지는 최초 감쇠 때 현재 weight로 고정(하위호환).
- 감쇠는 내용 변경이 아니므로 `updated_at`을 건드리지 않는다(staleness 기준점 보존).
- 문서별로 `write_lock` 아래 **최신 상태를 재로드**해 감쇠한다 — 패스가 도는 동안
  동시 추가된 엣지를 stale 스냅샷으로 덮어쓰지 않는다(→ [data-model.md](data-model.md)
  쓰기 직렬화).

## Review Queue와 judge (`patrol/review.rs`)

- 워크스페이스별 단일 JSON. 항목 상태: 대기 → 판단(valid / needs_fix / deleted / dismissed).
- 열린 동일 (문서, 유형) 항목은 중복 생성 금지. enqueue는 유형별 상한으로 폭주 방지.
- **judge는 멱등** — 같은 판단 재제출이 상태를 깨지 않고 부수효과도 이중 실행되지 않는다.
  부수효과를 상태 전이 **전에** 수행해 크래시 후 재제출로 복구 가능하다.
- 판단별 부수효과:
  - `valid` → freshness 기준점 갱신 (당분간 staleness 유예, `patrol/freshness.rs` —
    문서 raw JSON은 건드리지 않는다)
  - `deleted` → **복구 가능 삭제** (`Indexer::soft_delete_document`: 버전 스냅샷 보관 후 삭제)

## 피드백과 메트릭

- **피드백** (`patrol/feedback.rs`): 검색 결과 "관련 없음"(`POST /api/feedback`)을 일 단위
  JSONL로 축적(실패 무해), 문서별 집계가 staleness 신호에 가중된다.
- **메트릭** (`patrol/metrics.rs`): 일자 롤업 — 검색(횟수·zero-result율·평균 점수),
  그래프(노드·엣지·고아·평균 degree), 유입(문서 수·전략 분포), Patrol(탐지·큐 처리율).
  `workspaces/{id}/metrics/{YYYY-MM-DD}.json` 저장, 기간 조회는 `GET /api/metrics`.

## 스케줄러·이력

- 커넥터 스케줄러와 동일한 틱 루프 + 오류 격리 (`patrol/scheduler.rs`).
- `patrol.frequency`(hourly/daily/weekly)를 주기로 환산, 마지막 실행 이력
  (`patrol/history.rs`) 기준으로 due 판정. 수동 트리거는 동기 실행 후 리포트 반환.

## API 요약

| 작업 | 엔드포인트 | 권한 |
|------|-----------|------|
| 수동 실행 | `POST /api/patrol/run` | write |
| 실행 이력 | `GET /api/patrol/history` | read |
| 큐 조회 | `GET /api/review?status=&kind=` | read |
| 판단 | `POST /api/review/judge` (단건·일괄 통합) | write |
| 피드백 | `POST /api/feedback` | write |
| 메트릭 | `GET /api/metrics?from=&until=` (기본 30일) | read |

프론트엔드 `/review` 페이지가 이 API들의 UI다 (메트릭 카드 + 상태 필터 + 일괄 판정).
