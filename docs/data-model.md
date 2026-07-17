# 데이터 모델과 저장 구조

> 최종 검증: 2026-07-17 · 기준 커밋: 712484e · [문서 인덱스](README.md)

## 핵심 구조체 (`backend/src/models/document.rs`)

### Document

```rust
pub struct Document {
    pub id: Uuid,
    pub raw_content: String,            // 원문 — SSoT의 핵심
    pub summary: String,                // LLM 요약 (폴백 시 원문 발췌)
    pub entities: Vec<Entity>,          // 추출 엔티티
    pub facts: Vec<String>,             // 독립적 사실 문장 (청크 임베딩 대상)
    pub edges: Vec<Edge>,               // 그래프 간선 (#[serde(default)] — 하위호환)
    pub source: Option<DocumentSource>, // 커넥터 유입 메타 (직접 유입이면 None)
    pub created_at / updated_at: DateTime<Utc>,
}
```

### Edge / RelationType

```rust
pub struct Edge {
    pub target: Uuid,
    pub relation: RelationType,   // RelatedTo | Updates | Contradicts | References | PartOf
    pub weight: f32,              // [0,1] 클램프
    pub base_weight: Option<f32>, // 시간 감쇠의 기준값 (최초 감쇠 시 고정 — 멱등성 보장)
    pub created_at: DateTime<Utc>,
}
```

### Entity / DocumentSource

- `Entity`: `entity_type`(company/person/money/date/skill/project/location/other) + `value` + `context?`
- `DocumentSource`: `{source_type, source_id, modified_at, connector_id}` —
  커넥터 중복 방지 키(`(source_type, source_id)`)이자 출처 추적 수단. raw JSON에
  저장되므로 reindex에서 살아남는다.

## 디스크 레이아웃 (SSoT)

`DATA_DIR`(컨테이너 기본 `/data`) 하위. **`workspaces/` 트리와 루트의 두 JSON이
백업 대상의 전부**다 (→ [operations.md](operations.md)).

```
{DATA_DIR}/
├── settings.json                  # LLM provider 선택 + API 키 + codex 토큰
├── api_keys.json                  # 발급된 API 키 (SHA-256 해시만 저장)
├── models/                        # 로컬 임베딩 모델 캐시 (재다운로드 방지, 백업 불필요)
├── qdrant/                        # (compose 볼륨) 파생 인덱스 — reindex로 재생성 가능
└── workspaces/{id}/
    ├── config.json                # 워크스페이스 설정 (커넥터 인스턴스 등록 포함)
    ├── documents/{doc_id}.json    # ★ 문서 원장 (Single Source of Truth)
    ├── versions/{doc_id}/{ms}.json# 업데이트/삭제 전 스냅샷 (복구 안전망)
    ├── connectors/{conn_id}.json  # 커넥터 동기화 상태 (커서·마지막 결과)
    ├── search_logs/{날짜}.jsonl   # 검색 로그 (일 단위 append)
    ├── feedback/{날짜}.jsonl      # "관련 없음" 피드백
    ├── metrics/{날짜}.json        # 일자 메트릭 롤업
    └── patrol/                    # review_queue.json / freshness.json / state.json
```

## 인덱스 구조 — Atomic Fact 청킹

1 문서 → N Qdrant 포인트. 요약만으로 잡히지 않는 세부 사실도 개별 벡터로 매칭된다.

```
1 Document
├── 1 summary chunk  (embed(summary))    ← 항상 존재, edges를 payload에 비정규화
└── M fact chunks    (embed(fact[i]))    ← LLM이 추출한 독립 사실
```

Payload 필드 (`backend/src/storage/qdrant.rs`의 `build_chunk_payload`):
`document_id`(keyword 인덱스) · `chunk_type`("summary"|"fact", keyword 인덱스) ·
`chunk_index` · `chunk_text` · `summary` · `created_at` · `edges`(summary chunk에만, JSON 문자열).

## Qdrant 사용 규칙 (`backend/src/storage/qdrant.rs`)

- **컬렉션 네이밍**: 워크스페이스별 `documents_{workspace_id}`. 격리의 물리 단위.
- **차원 관리**: 임베딩 provider가 차원을 선언(gemini 3072 / openai 1536 / local 384)하고,
  provider 확보 시점에 `set_target_dim`으로 기대 차원을 동기화한다. 컬렉션 실제 차원과
  불일치하면 **자동 재생성 없이** 명시적 에러를 반환한다:
  `embedding dimension mismatch — run POST /api/reindex`
- **upsert 순서**: raw JSON(SSoT) 저장 성공 후에만 Qdrant를 호출한다. raw가 실패하면
  Qdrant는 호출조차 되지 않는다("둘 다 미반영"이 관측됨).
- **엣지 동기화**: 엣지 변경은 재임베딩 없이 summary chunk의 payload만 `set_payload`로
  갱신한다. 항상 raw JSON 먼저, payload는 best-effort.
- **삭제**: `document_id` 필터로 해당 문서의 전체 청크 일괄 삭제.

## 쓰기 직렬화 — lost-update·부활 방지

raw JSON은 엣지의 SSoT인데 저장은 파일 전체 덮어쓰기다. 따라서 `DocumentStore`의
모든 문서 쓰기 트랜잭션(load→수정→save)은 `write_lock`으로 직렬화된다
(`backend/src/storage/documents.rs`).

- 참여자: 엣지 감쇠 재계산, 엣지 추가/제거, 재파싱 업데이트, **삭제**(`delete_serialized`).
- 삭제가 락에 참여하지 않으면: 감쇠의 load→save 사이에 삭제가 파일을 지우고, 뒤늦은
  save가 삭제 문서를 **부활**시켜 SSoT와 Qdrant가 영구 불일치한다. 락 공유로 차단.
- 비용 관리: LLM 파싱·임베딩 같은 무거운 계산은 락 밖에서 끝내고, 임계 구역은
  "최신 재로드 → 저장"으로 짧게 유지한다. Qdrant 청크 삭제(파생물)도 임계 구역 밖.

## reindex — 파생 인덱스 전량 복원

`POST /api/reindex?workspace={id}` (`backend/src/core/indexer.rs::reindex_workspace`):

1. raw 문서 **전량** 로드 — 상한 없음(`usize::MAX`). 과거 10k 상한이 가장 오래된
   기억을 소리 없이 절단하던 버그는 제거됨(커밋 bf702c4).
2. 임베딩 provider 확보 → 현재 차원으로 `target_dim` 동기화 (차원 마이그레이션 지점).
3. 컬렉션 drop 후 현재 차원으로 재생성.
4. 문서별 재임베딩 + upsert. upsert 직전 `documents.exists()`를 재확인해 reindex 도중
   삭제된 문서의 **부활을 차단**하고 dangling 청크를 정리한다(커밋 508708b).
5. raw JSON의 edges가 summary chunk payload로 항상 복원된다 (단위 테스트로 고정).

## 버전 보관 (`backend/src/storage/versions.rs`)

- 업데이트·soft delete **전에** 문서 전체(엣지 포함)를
  `workspaces/{id}/versions/{doc_id}/{millis}.json`으로 스냅샷.
- archive 실패 시 업데이트 자체를 중단한다("이전 버전 보장" 시맨틱). 복원 UI는 백로그.

## 워크스페이스 (`backend/src/workspace/`)

- 격리 단위: 문서 디렉토리 + Qdrant 컬렉션 + `config.json` + API 키 스코프.
- `default`는 기동 시 자동 보장되고 삭제 불가. 레거시 flat 레이아웃(`data/raw/*.json`)은
  기동 시 `default`로 비파괴 마이그레이션된다.
- `config.json`: patrol(활성/주기/strictness), parsing, search(cross_workspace 목록,
  time_decay_lambda, deep_search 상한), connectors(인스턴스 등록) 설정을 담는다.
- 교차 검색: 설정의 `search.cross_workspace` ∩ 키 접근 권한 ∩ 실존 워크스페이스를
  대상으로 검색 후 점수로 병합·재정렬. 결과에 출처 `workspace`가 표시된다.

## 하위호환 규약

- 새 필드는 `#[serde(default)]` 또는 `Option` + `skip_serializing_if`로 추가한다 —
  구버전 raw JSON이 항상 로드 가능해야 한다(엣지·source가 그 선례).
- 응답 DTO도 동일 원칙: `agent`, `expanded_from` 등은 없으면 직렬화 생략.
