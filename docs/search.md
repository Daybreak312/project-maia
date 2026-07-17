# 검색 파이프라인

> 최종 검증: 2026-07-17 · 기준 커밋: 712484e · [문서 인덱스](README.md)

## 검색 모드 (`POST /search`)

| Mode | 방식 | 특징 |
|------|------|------|
| **hybrid** (기본) | Vector + Keyword 병렬 실행 → RRF 결합 | 의미 + 키워드 동시 고려 |
| **vector** | 쿼리 임베딩 → Qdrant 코사인 유사도 | 의미 유사 매칭 |
| **keyword** | summary chunk 대상 BM25 | 정확한 단어 일치 |

- **RRF 결합** (`backend/src/core/search.rs`): `1.0 / (k + rank + 1)`, `k = 60`.
  순위만 결합에 쓰고, **표시 점수는 벡터 검색의 raw cosine similarity**를 유지한다
  (정규화 점수가 전부 90%+로 보이는 오해 방지 — `contexts/decision_log.md` DEC-006).
- **품질 필터**: 절대 임계 0.5 + 점수 드롭 감지 0.15 + 기본 상한 5개.
- **벡터 검색 집계**: chunk 단위 검색 → `document_id`로 그룹핑 → 문서당 최고 점수 +
  `matched_facts` 수집.

## 한글 토크나이저 (`backend/src/core/search.rs::tokenize`)

한국어는 조사가 어간 뒤에 붙어 공백 분리만으로는 "해커톤에서"가 "해커톤" 질의와
매칭되지 않는다. 전략:

1. 공백·구두점으로 단어 분리.
2. 단어마다: 원본 전체 토큰 추가 + 스크립트 런(한글/영문) 분리 후 —
   한글 런은 길이 2 이상의 **접두사들**을 생성, 영문 런은 통짜 추가.
3. 예: `"해커톤에서"` → `["해커톤에서", "해커", "해커톤", ..., "에서"]`

DoS 방어 상한 (Phase 1에서 O(N²) 메모리 폭발 실측 후 도입):
- `MAX_HANGUL_PREFIX_LEN = 32` — 접두사 생성 최대 길이 (공백 없는 초장문 한글 런 방어)
- `MAX_TOKENS_PER_WORD = 128` — 단어당 토큰 상한 (dedup 전에 검사해 조기 반환)

쿼리 길이 하드컷은 의도적으로 없다 — 에이전트가 긴 블롭을 쿼리하는 것이 정상 패턴.

## 시간 인식 검색

- **기간 필터**: `since` / `until` (created_at 범위). 관련성 필터보다 먼저 적용.
- **시간 감쇠** (opt-in, `time_decay: true`): `exp(-λ · age_days)`로 **순위만** 재조정.
  표시 점수는 원본 유사도 유지 → "동일 유사도면 최신 우선". λ는 워크스페이스 설정
  `time_decay_lambda`.
- 처리 순서: 기간 필터 → 관련성 필터(원본 cosine) → 감쇠 재정렬. 오래됐지만 관련 있는
  문서가 임계값에서 사라지지 않는다.

## Deep Search — Search Agent (`agent: true`)

검색을 1회 조회에서 "충분해질 때까지 각도를 바꾸는 능동 회상"으로 확장한다. opt-in이며
미지정 시 기존 단일 검색 그대로다 (`backend/src/core/search_agent.rs`).

```
deep_search(query)
 ├─ 1. 초기 hybrid 검색 (LLM 무의존)
 ├─ 2. 재작성 루프: 충분성 평가(LLM, 보수적 — 애매하면 종료)
 │      └ 부족 시 쿼리 재작성(LLM) → 재검색, 결과 누적
 ├─ 3. 그래프 이웃 확장 (상위 결과의 이웃, 점수 = origin × edge_weight 감쇠)
 └─ 4. 합성: id 중복 제거(최고 점수, 동점 시 직접 결과 우선) → 재정렬 → 상위 N
```

**상한 불변식** — 루프는 반드시 종료한다:
- 재작성 ≤ 3회, 충분성+재작성 LLM 호출 ≤ 5회
- 시간 상한: 워크스페이스 `deep_search_time_limit_ms` (API 기본 15,000ms) — 초과 시 부분 결과
- 동일 쿼리 재검색 금지, 결과 총량 상한

**폴백 필수**: LLM 실패·미설정 시 초기 hybrid 결과를 **그대로** 반환하고(그래프 확장
생략) `fallback: true`. 초기 검색 자체가 실패하면 빈 결과 + 폴백 (500 금지).

**응답 메타데이터**: `mode: "agent"` + `agent: {rounds, queries, graph_expanded,
expansion_count, fallback, reason}`. 확장 유입 결과는 `expanded_from`으로 유래 표시.

## 그래프 이웃 탐색 (`GET /documents/{id}/neighbors`)

raw JSON 기반 BFS (`backend/src/storage/documents.rs::neighbors`):
- depth는 `[1, 5]` 클램프, 결과 상한 200개.
- `visited` 집합으로 순환 안전, 최단 depth 보장, dangling 엣지는 스킵.
- 각 이웃에 `depth / relation / via(부모) / weight`가 붙는다.

## 실패 처리와 관측

- **차원 불일치 사전 감지** (커밋 0fe6e03): hybrid 진입 전에 임베딩 provider 가용 시
  컬렉션 존재·차원을 대조해 명시적 에러를 올린다. "빈 결과"로 위장되지 않는다.
- 알려진 한계: 그 외의 벡터/키워드 개별 실패(Qdrant 순단 등)는 현재 로그 없이 남은
  팔로 강등된다 — [known-issues.md](known-issues.md) M4 참조.

## 검색 로그 (`backend/src/storage/search_log.rs`)

- 모든 검색(기존·agent)이 `workspaces/{id}/search_logs/{YYYY-MM-DD}.jsonl`에 append.
- 레코드: 시각·쿼리·모드·결과 수·최고 점수·zero-result 여부·소요 시간·(agent) 라운드 수.
- **실패 무해**: 기록 실패는 warn만 남기고 삼킨다 — 로그 장애가 검색을 실패시키지 않는다.
- Patrol 메트릭 롤업의 원천 데이터다 → [patrol.md](patrol.md).
