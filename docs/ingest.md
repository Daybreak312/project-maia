# 유입 파이프라인 — Ingest Agent & 커넥터

> 최종 검증: 2026-07-17 · 기준 커밋: 712484e · [문서 인덱스](README.md)

## 정보 유실 0 — 구현 지점

모든 유입 경로는 판단·파싱·임베딩 **이전에** raw 문서를 SSoT(파일시스템)에 먼저
기록한다 (`backend/src/core/indexer.rs::persist_raw_document`). 이후 어느 단계가
실패해도:

- LLM provider 미확보 → raw 폴백
- 에이전트 판단 실패(재시도 소진) → raw 폴백
- 전략 실행 실패 → raw 폴백

폴백은 에러가 아니라 정상 응답이며 `fallback: true` + `strategy: "raw"`로 표시된다.
폴백 요약은 원문 발췌로 만들고, 폴백 경로 자체는 LLM을 다시 호출하지 않는다.

## 기본 파이프라인 (`POST /ingest?mode=raw` 또는 내부 실행기)

```
원문 → ① raw 저장(SSoT) → ② LLM 파싱(summary/entities/facts)
     → ③ 청크 임베딩(summary + facts) → ④ Qdrant upsert
```

`?mode=raw`는 에이전트 판단을 명시적으로 우회한다(②의 LLM 파싱 없이 폴백 요약).

## Smart Ingest — 저장 판단 (기본 모드)

저장을 "받아쓰기"에서 "이해하고 정리하기"로 바꾼다
(`backend/src/core/indexer.rs::smart_ingest_to_workspace` + `core/ingest_agent.rs`).

```
smart_ingest(raw)
 ├─ 1. 관련 후보 검색 (임베딩 유사도 상위 5, 요약만 전달)
 ├─ 2. IngestAgent.decide → 전략 판단 (LLM, 재시도 1회)
 │      └ 파싱 실패·환각 target·무의미 분할은 안전 강등/폴백
 ├─ 3. 전략 실행 (기존 인덱싱 파이프라인 재사용)
 │      ├ new:       신규 저장
 │      ├ update:    기존 원문 + 새 원문 append 재파싱 (이전 버전 보관)
 │      ├ split:     N개 분할 저장 (상한 5, 실패 시 원문 전체 raw 보존)
 │      └ duplicate: 원문 보관 + 원본과 엣지 연결 (삭제 판단은 사람 몫)
 └─ 4. 자동 엣지 생성 (관계 타입 LLM 판단, 실패 시 RELATED_TO 폴백)
```

- **LLM 호출 상수 상한**: New/Update = 판단 1 + 관계 판단 1 = 2회.
  Split/Duplicate = 1회. 세그먼트 수에 비례한 호출은 금지.
- **응답** `IngestOutcome`: 기존 `IngestResponse`(id/summary/entities/facts)의 상위집합 +
  `strategy / document_ids / edges_created / fallback / reason` (하위호환).

## 커넥터 시스템 (`backend/src/connectors/`)

소유자의 지식이 사는 로컬 마크다운 생태계에서 Maia로 정보가 스스로 흘러들어오는
유입 계층. 모든 유입은 소스 식별자 기반 중복 방지를 거친다.

```
스케줄러(기본 틱 30s) ─┐
                      ├─▶ Runner.run_sync ─▶ Connector.fetch_changes(cursor) ─▶ 변경 항목들
API 트리거(수동) ──────┘        │   동시성 제한(buffer_unordered) + 항목별 태스크 격리
                               └─▶ Indexer.ingest_item ─▶ (source_type, source_id) dedup
                                        ├ 미존재          → 신규 Created
                                        ├ 원본이 더 최신   → 업데이트 Updated (edges·created_at 보존)
                                        └ 변경 없음        → Skipped
```

### 공통 계약 (`connectors/mod.rs`)

- `Connector` trait: `source_type()` + `fetch_changes(cursor) -> {items, next_cursor}`.
  커서 의미는 커넥터가 온전히 소유한다(불투명 문자열). 새 타입은 trait 구현 +
  `build_connector` 팩토리 한 줄로 열린다.
- **1파일 = 1문서** 매핑: 재유입 시 문서 난립을 원천 차단 (분할 대신 업데이트 결정성).
- 유입 모드:
  - `parsed`(기본) — LLM 파싱 + 자동 그래프 연결. 품질 우선.
  - `raw` — LLM 없이 원문 저장(폴백 요약). rate limit 보호·대량 적재 우선.

### 로컬 디렉토리 커넥터 (`connectors/local_dir.rs`)

- 설정(`LocalDirectoryConfig`): `directories[]` / `extensions[]`(대소문자 무시) /
  `exclude[]`(glob) / `max_file_bytes`.
- 커서 = RFC3339 스캔 시각. mtime > 커서인 파일만 유입. 손상 커서는 경고 후 전체 스캔.
- **심볼릭 링크 방어**: 등록 루트만 canonicalize로 따르고, 순회 중 발견된 링크는 따르지
  않는다(등록 범위 밖 읽기 차단). 깨진 인코딩·읽기 실패는 스킵·기록(장애 격리). 읽기 전용.

### 러너·커서 시맨틱 (`connectors/runner.rs`)

- 동시성 제한(`buffer_unordered`) + 항목별 `tokio::spawn`으로 패닉까지 격리.
- 인메모리 진행 상태를 항목 완료마다 갱신 → 상태 API로 실시간 관측.
- 동시 실행 방지: 같은 커넥터 이중 실행은 409, 같은 소스 동시 유입은 in-flight 가드.
- **커서는 실패 0일 때만 전진.** 중단·부분 실패 시 다음 실행이 재스캔하고, 이미 유입된
  항목은 소스 dedup으로 Skipped 처리되어 이어서 진행된다(중단 재개·정보 유실 0).
- 대량 적재 = 커서 무시 `full` 동기화 (같은 코드 경로).

### 동기화 상태 (`connectors/sync_state.rs`)

`workspaces/{ws}/connectors/{id}.json`에 마지막 실행 시각·커서·결과 요약
(processed/created/updated/skipped/failed + 실패 목록, 상한 50건)을 영속화.

### 알려진 제약 — poison item

`parsed` 모드에서 **결정적으로** 파싱 불가한 항목(콘텐츠 필터 거절, 출력 상한 초과 등)이
있으면 커서가 전진하지 못해 매 주기 재스캔 + 재파싱 호출이 반복된다. 원문 파일이
남아 있어 유실은 없지만 쿼터를 소모한다. 자동 검역(dead-letter)은 미구현 —
[known-issues.md](known-issues.md) M3 참조. 급할 때의 우회는 `mode=raw` 동기화
(→ [operations.md](operations.md)).

## API 요약

전체 스키마는 [api.md](api.md) 참조.

| 작업 | 엔드포인트 | 비고 |
|------|-----------|------|
| 직접 유입 | `POST /ingest?workspace=&mode=` | 기본 smart, `mode=raw`로 우회 |
| 커넥터 목록/상태 | `GET /api/connectors` / `GET /api/connectors/{id}/status` | 진행 중이면 실시간 progress 포함 |
| 커넥터 등록/삭제 | `POST /api/connectors` / `DELETE /api/connectors/{id}` | admin. 삭제해도 유입 문서는 보존 |
| 즉시 동기화 | `POST /api/connectors/{id}/sync` | admin, body `{mode, full, concurrency}`, 202 + 백그라운드 |
