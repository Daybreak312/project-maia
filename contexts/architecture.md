# System Architecture

## Monorepo Structure

```
project-maia/
├── backend/                    # Rust RAG 서버
│   ├── Cargo.toml
│   ├── src/
│   │   ├── main.rs             # 진입점, axum 서버, AppState(indexer/settings/workspaces/api_keys/connector_runner/patrol) + 커넥터·Patrol 스케줄러 기동
│   │   ├── config.rs           # 환경설정 (SERVER_PORT, QDRANT_URL, DATA_DIR, MAIA_API_KEY)
│   │   ├── settings.rs         # 설정 관리 (LLM API Key, Provider 선택)
│   │   │
│   │   ├── auth/               # 인증/인가
│   │   │   ├── mod.rs          # require_api_key 미들웨어 (마스터키→ApiKeyManager→401, AuthContext 주입)
│   │   │   └── keys.rs         # ApiKey, ApiKeyManager, AuthContext, Permission (SHA-256 해시)
│   │   │
│   │   ├── workspace/          # 워크스페이스 시스템
│   │   │   ├── mod.rs
│   │   │   ├── config.rs       # WorkspaceConfig (patrol/parsing/search/connectors + cross_workspace), 템플릿
│   │   │   ├── connector_config.rs # ConnectorInstance/ConnectorSpec/LocalDirectoryConfig (커넥터 등록 스키마)
│   │   │   └── manager.rs      # WorkspaceManager CRUD, default 보호, 레거시 마이그레이션
│   │   │
│   │   ├── llm/                # AI Provider 추상화 레이어
│   │   │   ├── mod.rs          # LlmProvider, EmbeddingProvider traits
│   │   │   ├── gemini.rs       # Gemini 구현
│   │   │   ├── claude.rs       # Claude 구현
│   │   │   └── openai.rs       # OpenAI 구현
│   │   │
│   │   ├── api/                # HTTP API 레이어 (전 엔드포인트 워크스페이스 인식)
│   │   │   ├── mod.rs          # WorkspaceQuery, resolve_and_authorize_workspace, require_admin/write
│   │   │   ├── ingest.rs       # POST /ingest?workspace=&mode= (에이전트/raw)
│   │   │   ├── search.rs       # POST /search?workspace= (교차검색·시간 인식)
│   │   │   ├── documents.rs    # CRUD /documents, GET /recent, POST /api/reindex (workspace 스코프)
│   │   │   ├── graph.rs        # 이웃 조회 GET /documents/:id/neighbors, 수동 엣지 추가/삭제
│   │   │   ├── workspaces.rs   # 워크스페이스 CRUD API (admin 전용)
│   │   │   ├── connectors.rs   # 커넥터 관리 API (목록/상태=워크스페이스 접근, 등록/삭제/즉시실행=admin)
│   │   │   ├── keys.rs         # API 키 발급/조회/폐기 API (admin 전용)
│   │   │   ├── patrol.rs       # Patrol·거버넌스 API (실행/판단/피드백=write, 이력/큐/메트릭 조회=접근)
│   │   │   └── settings.rs     # 설정 API (mutation은 admin)
│   │   │
│   │   ├── core/               # 비즈니스 로직
│   │   │   ├── mod.rs
│   │   │   ├── indexer.rs      # 인덱싱·검색·smart_ingest·deep_search 오케스트레이션 + SearchBackend 구현
│   │   │   ├── ingest_agent.rs # Ingest Agent: 신규/업데이트/분할/중복 판단 + 관계 판단
│   │   │   ├── search_agent.rs # Search Agent: 충분성 평가·쿼리 재작성·그래프 확장·합성 (SearchBackend trait)
│   │   │   └── search.rs       # BM25, RRF, 하이브리드 검색
│   │   │
│   │   ├── connectors/         # 유입 파이프라인 (Phase 4)
│   │   │   ├── mod.rs          # Connector/ConnectorIngest trait, ConnectorItem, build_connector 팩토리
│   │   │   ├── local_dir.rs    # 로컬 디렉토리 커넥터 (증분 스캔·확장자/제외·크기 상한·심볼릭 링크 방어)
│   │   │   ├── sync_state.rs   # 커넥터별 마지막 실행·커서·결과 요약 영속화
│   │   │   ├── runner.rs       # 동기화/대량 적재 (동시성 제한·진행 관측·실패 격리·중단 재개)
│   │   │   └── scheduler.rs    # 주기 실행 + 오류/패닉 격리 (기동 시 자동 시작)
│   │   │
│   │   ├── patrol/             # 자기 관리 & 메모리 거버넌스 (Phase 5)
│   │   │   ├── mod.rs          # Patrol 파사드(run/judge/조회/피드백) + PatrolExecutor trait
│   │   │   ├── detectors.rs    # staleness/중복/고아/외부 불일치 4종 (LLM 없이 수치 신호, 순수)
│   │   │   ├── decay.rs        # 엣지 시간 감쇠 (멱등, base_weight 기준 재계산, 순수)
│   │   │   ├── review.rs       # Review Queue 모델·저장·중복 방지·멱등 판단
│   │   │   ├── freshness.rs    # "유효" 판단 기준점 (문서 미변경 — 별도 파일)
│   │   │   ├── feedback.rs     # "관련 없음" 피드백 일 JSONL + 문서별 집계
│   │   │   ├── metrics.rs      # 일 롤업(검색/그래프/유입/Patrol) 순수 계산·저장
│   │   │   ├── history.rs      # 실행 이력·마지막 실행 시각 (스케줄 due 판정)
│   │   │   └── scheduler.rs    # 주기 실행 + 오류 격리 (frequency→주기)
│   │   │
│   │   ├── storage/            # 데이터 레이어
│   │   │   ├── mod.rs
│   │   │   ├── qdrant.rs       # Qdrant 벡터 검색/저장, 엣지 payload 비정규화
│   │   │   ├── documents.rs    # 원본 문서 파일 저장, 그래프 이웃 BFS 탐색
│   │   │   ├── versions.rs     # 업데이트 시 이전 버전 스냅샷 보관
│   │   │   └── search_log.rs   # 검색 로그 워크스페이스별 일 단위 JSONL 축적 (실패 무해)
│   │   │
│   │   └── models/
│   │       ├── mod.rs
│   │       └── document.rs     # Document(+edges, +source), Entity, Edge/RelationType, DocumentSource, API DTO
│   │
│   ├── static/                 # 레거시 정적 프론트엔드 (Vanilla HTML)
│   ├── data/                   # 런타임 데이터 (Volume)
│   │   ├── workspaces/{id}/    # 워크스페이스별 격리 저장
│   │   │   ├── config.json     #   워크스페이스 설정 (커넥터 인스턴스 등록 포함)
│   │   │   ├── documents/      #   원본 문서 JSON (Single Source of Truth)
│   │   │   ├── versions/       #   업데이트/삭제 전 스냅샷 (복구 안전망)
│   │   │   ├── search_logs/    #   검색 로그 일 단위 JSONL (메트릭·거버넌스 신호)
│   │   │   ├── connectors/     #   커넥터별 동기화 상태 {connector_id}.json (마지막 실행·커서·결과)
│   │   │   ├── feedback/       #   "관련 없음" 피드백 일 단위 JSONL (Phase 5)
│   │   │   ├── metrics/        #   일자 메트릭 롤업 {YYYY-MM-DD}.json (Phase 5)
│   │   │   └── patrol/         #   Review Queue·freshness·실행 이력 (review_queue.json/freshness.json/state.json)
│   │   ├── raw/                # 레거시 flat 문서 (기동 시 default로 마이그레이션)
│   │   ├── api_keys.json       # API 키(해시만) 저장
│   │   ├── settings.json       # LLM API Key 및 Provider 설정
│   │   └── qdrant/             # Qdrant 벡터 DB 데이터 (documents_{id} 컬렉션)
│   │
│   ├── Dockerfile
│   ├── docker-compose.yml
│   └── .env.example
│
├── mcp/                        # MCP 브릿지 서버 (TypeScript)
│   ├── src/
│   │   ├── index.ts            # MCP 서버 진입점 + Tool 정의
│   │   └── maia-client.ts      # Maia REST API HTTP 클라이언트
│   ├── dist/                   # 빌드 결과
│   ├── package.json
│   └── tsconfig.json
│
├── frontend/                   # React + Vite 프론트엔드
│   ├── src/
│   │   ├── App.tsx
│   │   ├── components/         # Navbar, Toast, Pagination 등
│   │   └── pages/              # AddPage, SearchPage, BrowsePage, AdminPage
│   ├── public/
│   │   └── logo.svg
│   └── package.json
│
├── contexts/                   # 프로젝트 컨텍스트 문서
└── CLAUDE.md
```

## Tech Stack

| Layer | Technology | Rationale |
|-------|------------|-----------|
| Backend Language | **Rust** | 단일 바이너리, 성능, 타입 안전성 |
| HTTP Server | **axum 0.7** | tokio 기반, 미들웨어 체계 |
| Vector DB | **Qdrant** | Rust 네이티브 클라이언트, 필터링, Docker 지원 |
| LLM (파싱) | **Gemini/Claude(API키·구독 OAuth)/OpenAI/Codex(구독)** | Provider 패턴으로 교체 가능, 구독 기반으로 종량제 키 탈피 |
| Embedding | **Gemini/OpenAI/Local(fastembed)** | 로컬 임베딩(multilingual-e5-small)으로 외부 키 없이 자립 |
| Local Embedding | **fastembed 5 + ort(onnxruntime)** | 정적 링크 onnxruntime, 모델은 DATA_DIR/models 캐시 |
| MCP Server | **TypeScript** | MCP SDK 생태계 가장 성숙, STDIO transport |
| Frontend | **React + Vite** | SPA, 컴포넌트 기반 |
| Container | **Docker** | 이식성, 볼륨으로 데이터 분리 |

## System Topology

```
┌─ 로컬 머신 ────────────────────────────┐     ┌─ EC2 / 온프레미스 ──────────┐
│                                         │     │                              │
│  Claude Desktop / Cursor / Gemini CLI   │     │  Maia Backend (Rust, :8080)  │
│       │                                 │     │       │                      │
│       ▼                                 │     │       ▼                      │
│  MCP Server (STDIO, Node.js)  ─── HTTP ──────▶  REST API                    │
│                                         │     │  (Authorization: Bearer)     │
│  Frontend (React, 개발/관리용)          │     │       │                      │
│                                         │     │       ▼                      │
└─────────────────────────────────────────┘     │  Qdrant (:6333/:6334)        │
                                                └──────────────────────────────┘
```

## Authentication & Authorization

`require_api_key` 미들웨어(`auth/mod.rs`)가 Bearer 토큰을 다음 순서로 해석한다:

1. **마스터키** (`MAIA_API_KEY`, 상수 시간 비교) → 전체 admin `AuthContext`
2. **등록 키** (`ApiKeyManager.authenticate`: SHA-256 해시 조회 + 만료 체크) → 키 스코프 `AuthContext`
3. 어느 것과도 불일치 → **401**

- 검증 성공 시 `AuthContext`를 request extension으로 주입 → 핸들러가 워크스페이스 접근·권한 판단.
- `MAIA_API_KEY` 미설정 시 인증 비활성(개발 모드, dev `AuthContext`=admin). `/health`는 항상 공개.
- 인증 성공 시 등록 키의 `last_used_at`을 `tokio::spawn`으로 비블로킹 갱신(요청 경로 무영향).
  요청당 파일 재작성(쓰기 증폭)을 막기 위해 최소 간격(60s) 안의 갱신은 디스크에 반영하지 않는다.

### Permission (3단계)
| 권한 | 문서 읽기 | 문서 쓰기 | 워크스페이스·키 관리 |
|------|----------|----------|---------------------|
| `read_only`  | ✅ | ❌ | ❌ |
| `read_write` | ✅ | ✅ | ❌ |
| `admin`      | ✅ | ✅ | ✅ |

### API Key 모델 (`data/api_keys.json`)
- 저장: `key_id`, `hashed_key`(SHA-256, 평문 미저장), `label`, `workspaces[]`, `permissions`, `created_at`, `last_used_at`, `expires_at?`
- 발급 API 응답에서만 평문 키 1회 노출. 목록 조회는 해시를 제외한 뷰 반환.
- **워크스페이스 스코핑 (fail-closed):** 영속 키는 `workspaces[]`에 명시된 워크스페이스에만 접근한다.
  빈 목록은 "전체 접근"이 아니라 **접근 없음**이며(`can_access_workspace`), 발급 시 최소 1개 워크스페이스를
  요구한다(빈 스코프는 400). "unscoped = all"은 마스터키(env) 전용 의미다.
- **영속화:** `save()`는 temp 파일 쓰기 후 `rename`으로 원자적 교체(torn write → 부팅 브릭 방지),
  동시 저장은 `save_lock`으로 직렬화. 기동 시 파싱 실패하면 하드 실패 대신 손상본을 `.corrupt`로
  백업하고 빈 목록으로 degrade(마스터키로 복구 가능).

## Workspace System

격리된 지식 공간. 컬렉션·파일·키 스코프의 단위.

- **격리:** 워크스페이스별 Qdrant 컬렉션 `documents_{id}` + 파일 경로 `data/workspaces/{id}/documents/`.
  동일 문서 ID가 서로 다른 워크스페이스에 충돌 없이 공존 가능.
- **라우팅:** 모든 문서/검색/인제스트 엔드포인트가 `?workspace=` 파라미터 수용.
  미지정 시 키에 바인딩된 기본 워크스페이스(마스터/개발모드는 `default`).
  존재하지 않으면 404, 접근 불가면 403.
- **CRUD:** admin 키만 생성/삭제. 생성 시 컬렉션 준비, 삭제 시 raw 문서·컬렉션·설정 정리(best-effort).
  `default`는 삭제 불가. Qdrant 불가용 시에도 파일 기반 관리 API는 동작.
- **교차 검색:** 워크스페이스 설정 `search.cross_workspace` 목록 ∩ 키 접근 권한 ∩ 존재하는 워크스페이스를
  대상으로 검색 후 relevance_score로 병합·재정렬. 각 결과에 출처 `workspace` 표시.
- **레거시 마이그레이션:** 기동 시 `ensure_default`가 `data/raw/*.json`(구 flat 레이아웃)을
  `data/workspaces/default/documents/`로 복사(비파괴적).

## LLM Provider Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                      LlmProvider (trait)                     │
│  - parse(content) -> ParsedContent   (고정 스키마 파싱)     │
│  - complete(prompt) -> String        (자유 형식, 에이전트 판단)│
│  - validate_api_key() -> bool                               │
├──────────────┬──────────────┬───────────────┬───────────────┤
│ GeminiProvider│ ClaudeProvider│ OpenAiChat    │ CodexProvider │
│              │ (API키/OAuth) │               │ (구독 OAuth)  │
└──────────────┴──────────────┴───────────────┴───────────────┘

┌─────────────────────────────────────────────────────────────┐
│                   EmbeddingProvider (trait)                  │
│  - embed(text) / embed_query(text) -> Vec<f32>              │
│  - dimension() -> usize                                     │
├──────────────────┬──────────────────┬───────────────────────┤
│ GeminiEmbedding  │ OpenAiEmbedding  │ LocalEmbedding        │
│ (3072 dim)       │ (1536 dim)       │ (384 dim, multilingual│
│                  │                  │  -e5-small, 로컬)     │
└──────────────────┴──────────────────┴───────────────────────┘
```

`ProviderType` = {gemini, claude, openai, **codex**, **local**}. 교차 선택 제약은
`valid_for_parsing`/`valid_for_embedding` 단일 지점으로 판정한다: **local은 임베딩
전용**(파싱 불가), **codex는 파싱 전용**(임베딩 불가), claude는 임베딩 미지원. 위반 시
설정 API가 400을 반환한다.

### 구독 프로바이더 (Phase 6 — 종량제 키 탈피)

- **Claude OAuth 모드**: 키 접두 자동 감지 — `sk-ant-oat…`(`claude setup-token` 산출물)
  이면 OAuth 모드로 `Authorization: Bearer` + `anthropic-beta: oauth-2025-04-20` 헤더를
  쓰고 `x-api-key`를 제거하며, Claude Code system 프리픽스를 부여한다. `sk-ant-api…`는
  기존 `x-api-key` 경로 그대로(회귀 없음). 파싱 모델 상수 `claude-haiku-4-5`.
- **Codex 프로바이더**(신규, 파싱 전용): `~/.codex/auth.json` 임포트로 활성화. access
  token JWT `exp` 임박(여유 60초) 또는 401 시 `auth.openai.com/oauth/token`으로 **단일
  플라이트** refresh(동시 파싱 호출이 중복 refresh하지 않음), 회전된 refresh_token 영속화,
  실패 시 "재임포트 필요" 명시. 파싱은 `chatgpt.com/backend-api/codex/responses`(Responses
  API, SSE 관대 집계, 모델 `gpt-5.1`). 비공식 업스트림 상수(엔드포인트·client_id·헤더)는
  `llm/codex.rs`의 단일 모듈에 격리(출처·검증일 주석). 토큰은 Debug/에러/응답에서 앞4+뒤4 마스킹.
- **로컬 임베딩**(신규, 임베딩 전용): fastembed `multilingual-e5-small`(384d, 다국어).
  모델은 `DATA_DIR/models`에 캐시(도커 볼륨 영속), 첫 embed 호출 시 lazy 로드/다운로드.
  e5 규약대로 문서는 `passage:`, 쿼리는 `query:` 접두(`embed`/`embed_query` 분리). 키 불요.
  - **컨테이너 빌드 제약**(실측 2026-07-07, 리스크 1순위 해소): 프리빌트 onnxruntime(aarch64)이
    glibc 2.38+ 심볼(`__isoc23_*`)을 요구해 빌더·런타임 베이스는 **debian trixie**여야 한다
    (bookworm(glibc 2.36)은 최종 링크 실패). GCC12+에서 제거된 `__cxa_call_terminate`는
    `backend/docker/onnx_compat.c` 호환 심으로 채운다. 런타임 의존: `libstdc++6`·`libgomp1`·`libssl3`.
    컨테이너 E2E(이미지 빌드 + in-container 임베딩 384d + 볼륨 캐시 재사용) 검증 완료.
    상세 근거는 `Dockerfile` Stage 2 및 `onnx_compat.c` 주석.

### 임베딩 차원 메타 & 마이그레이션

각 임베딩 provider가 차원을 선언(gemini 3072 / openai 1536 / local 384)한다.
`QdrantStorage`는 현재 provider 차원(`target_dim`)을 기억하고, 컬렉션 존재 시 실제 차원과
대조한다. **불일치면 침묵 실패·자동 재생성 없이** `embedding dimension mismatch — run
POST /api/reindex` 에러를 반환(검색·유입 경로). `POST /api/reindex`가 현재 차원으로 컬렉션을
재생성하고 raw JSON(SSoT) 전량을 재임베딩해 전환(예: 3072→384)을 완결한다(문서 손실 0).

**정보 유실 0 불변식 유지**: 파싱·임베딩·refresh가 전부 실패해도 raw 저장은 성공한다
(기존 raw 폴백 경로 불변).

## MCP Tools

| Tool | Description | Maia API |
|------|-------------|----------|
| `search_context` | 개인 지식 베이스 검색 (1회성) | `POST /search` |
| `deep_search` | 능동 회상 (충분성 평가·재작성·그래프 확장) | `POST /search?agent=true` (body `agent:true`) |
| `ingest_information` | 새 정보 저장 (에이전트 전략 표시) | `POST /ingest` |
| `get_document` | 문서 원문 조회 | `GET /documents/{id}` |
| `list_recent_documents` | 최근 문서 목록 | `GET /recent` |
| `get_neighbors` | 그래프 이웃(연결 문서) 조회 | `GET /documents/{id}/neighbors` |

모든 tool은 선택적 `workspace` 인자를 받는다. 미지정 시 `MAIA_WORKSPACE` 환경변수,
그것도 없으면 서버가 API 키의 기본 워크스페이스를 사용한다.
`ingest_information` 응답에는 에이전트 판단 전략(new/update/split/duplicate/raw)과
폴백 여부가 표시된다. `deep_search` 응답에는 탐색 과정 요약(라운드 수·시도 쿼리·그래프
확장 여부·폴백 여부)과 각 확장 결과의 유래(`expanded_from`)가 포함된다.

## Indexing Architecture (Atomic Fact Chunking)

```
1 Document → N Qdrant Points
├── 1 summary chunk (embed(summary))       ← 항상 존재, edges 비정규화 저장
└── M fact chunks   (embed(fact[0..M-1]))  ← LLM이 추출한 독립적 사실

Point Payload: { document_id, chunk_type, chunk_index, chunk_text, summary, created_at }
  + summary chunk에만: edges (raw JSON의 그래프 엣지를 JSON 문자열로 비정규화)
Payload Indexes: document_id (Keyword), chunk_type (Keyword)
```

**Vector 검색**: chunk 단위 검색 → `document_id` 기준 그룹핑 → 최고 점수 + matched_facts 수집
**Keyword 검색**: `chunk_type="summary"` chunk만 대상 → BM25 스코어링
**Hybrid 검색**: Vector + Keyword 결과를 RRF로 순서 결합, raw cosine similarity 점수 유지

**검색 품질 필터링**: 절대 임계값(0.5) + 점수 드롭 감지(0.15) + 최대 결과(5개)

## Knowledge Graph (지식 그래프)

문서 간 관계를 데이터 모델의 일급 시민으로 다룬다.

- **Edge 모델** (`models/document.rs`): 방향성 엣지 `{ target, relation, weight(0~1), created_at }`.
  관계 타입 `RelationType`: RELATED_TO / UPDATES / CONTRADICTS / REFERENCES / PART_OF (확장 가능).
- **SSoT = raw JSON**: 엣지는 `Document.edges`(raw JSON)에 저장된다. `#[serde(default)]`로
  엣지 없는 구버전 JSON도 로드된다(하위호환).
- **Qdrant 비정규화**: 인덱싱 시 summary chunk payload에 엣지를 JSON으로 비정규화한다(문서당 1곳).
  `set_payload`로 엣지만 재동기화(재임베딩 없음). **reindex가 raw JSON의 엣지를 payload로 항상
  복원**하는 것이 핵심 불변식(단위 테스트로 고정: raw 보존 + payload 왕복 + build_chunk_payload).
- **엣지 동기 갱신 순서**: raw JSON을 먼저 저장하고 성공 시에만 payload를 동기화한다.
  raw 실패 시 Qdrant는 호출조차 되지 않아 "둘 다 미반영"이 관측된다.
- **쓰기 직렬화 (lost-update·부활 방지)**: `DocumentStore`의 문서 쓰기 트랜잭션(load→수정→save)은
  `write_lock`으로 직렬화된다(`DocumentStore::update` 동기 클로저 / `write_guard` 복합 트랜잭션 /
  `delete_serialized` 삭제). raw JSON이 엣지의 SSoT인데 `save`는 락 없는 전체 덮어쓰기라, **엣지 감쇠
  재계산·엣지 추가/제거·재파싱 업데이트**가 같은 문서를 동시에 write하면 늦은 쪽이 앞선 엣지를 조용히
  소실시킨다(reindex도 오염된 raw를 읽어 복원 불가 — "기억을 잃으면 안 된다" 위반). **삭제도 이 락에
  참여한다**(`delete_serialized`): 참여하지 않으면 감쇠/엣지추가의 load→save 사이에 삭제가 파일을 지우고
  뒤늦은 save가 삭제 문서를 raw JSON에 **부활**시켜 SSoT(살아있음)와 Qdrant(삭제됨)가 영구 불일치하고
  reindex가 소유자가 파기한 지식을 되살린다("삭제=삭제" 붕괴). 생성·수정·삭제 **모든 라이터가 이 락을
  공유**해 경합을 제거한다(`update`의 "락 아래 exists 체크"가 삭제와 갱신을 상호 배제 — 삭제가 먼저면
  save 생략, 갱신이 먼저면 삭제가 방금 저장된 파일 제거). 감쇠·업데이트는 계산(LLM 파싱·임베딩)을 락
  밖에서 끝내고 임계 구역은 최신 재로드→저장으로 짧게 유지하며, 삭제도 Qdrant 제거(파생물)를 임계 구역
  밖에 둔다. review/freshness/history 저장소와 동일한 파일 쓰기 직렬화 패턴.
- **이웃 탐색** (`storage/documents.rs::neighbors`): raw JSON 기반 BFS. depth `[1, 5]` 클램프,
  결과 200개 상한, `visited` 집합으로 순환 안전, 최단 depth 보장, dangling 엣지 스킵.

## Ingest Agent (저장 판단)

저장을 "받아쓰기"에서 "이해하고 정리하기"로 전환한다. LLM 판단은 `LlmProvider::complete` 경유.

```
smart_ingest(raw)
  ├─ 1. 관련 후보 검색 (임베딩 유사도 상위 소수, 요약만)
  ├─ 2. IngestAgent.decide → 전략 (new/update/split/duplicate)
  │       └ 재시도 1회, 파싱 실패·환각 target·무의미 분할은 안전 강등/폴백
  ├─ 3. 전략 실행 (기존 인덱싱 파이프라인을 실행기로 재사용)
  │       ├ new:       신규 저장
  │       ├ update:    기존 원문+새 원문 append 재파싱 (이전 버전 보관)
  │       ├ split:     N개로 분할 저장 (상한 5)
  │       └ duplicate: 원문 보관 + 원본과 엣지 연결 (삭제는 Phase 5 사람 몫)
  └─ 4. 자동 엣지 생성 (관계 타입 LLM 판단, 실패 시 RELATED_TO)
```

- **정보 유실 0**: 어느 단계든 실패하면 raw 저장으로 폴백하고 응답에 `fallback=true` 표시.
  `POST /ingest?mode=raw`는 에이전트를 명시적으로 우회(fallback=false).
- **LLM 호출 상수 상한**: New/Update = 판단 1 + 관계 판단 1 = 2회. Split/Duplicate = 1회
  (세그먼트 수 비례 호출 금지).
- **응답**: `IngestOutcome` = 기존 IngestResponse(id/summary/entities/facts) 상위집합
  + `strategy / document_ids / edges_created / fallback / reason` (하위호환).
- **테스트**: 프롬프트 빌더·응답 파서는 순수 함수, `decide`/`judge_relations`는 mock provider로
  각 분기·강등·재시도·폴백을 고정.

## Search Agent (검색 회상)

검색을 1회성 조회에서, 충분해질 때까지 스스로 각도를 바꿔 탐색하는 능동 프로세스로 바꾼다.
opt-in — `POST /search` body에 `agent:true`면 활성화, 미지정 시 기존 단일 검색 그대로.

```
deep_search(query)
  ├─ 1. 초기 hybrid 검색 (LLM 무의존, 기존 교차 워크스페이스 파이프라인 재사용)
  ├─ 2. 재작성 루프 (상한: 재작성 3회 / LLM 5회 / 시간 상한 / 동일 쿼리 금지)
  │      ├ 충분성 평가 (LLM) — 보수적: 애매하면 '충분'으로 조기 종료
  │      └ [부족] 쿼리 재작성 (LLM) → 재검색, 결과 누적
  ├─ 3. 그래프 이웃 확장 (폴백 아닐 때만, 상위 결과의 이웃, depth=워크스페이스 설정)
  │      └ 확장 결과는 expanded_from으로 유래 표시, 점수는 origin×edge_weight로 감쇠
  └─ 4. 합성: id 중복 제거(최고 점수·동점 시 직접 우선) → 점수 재정렬 → 상위 N 절사
```

- **판단·합성 분리** (`core/search_agent.rs`): 충분성 평가·재작성은 `LlmProvider` 경유
  (프롬프트 중앙화), 합성·확장 점수는 순수 함수. 실제 검색·확장 I/O는 `SearchBackend`
  trait로 주입 — `Indexer`가 구현하고, 단위 테스트는 mock backend + mock LLM으로 전
  분기(충분/부족/재작성/확장/합성/폴백/상한)를 고정한다(Qdrant·실 LLM 무의존).
- **폴백 필수**: LLM 실패·미설정 시 초기 hybrid 결과를 **그대로** 반환하고(그래프 확장
  생략) `fallback=true` 표시. 에러가 아니라 결과 + 폴백. 초기 검색 자체가 실패하면 빈
  결과 + 폴백(500 금지).
- **상한 불변식**: 재작성 ≤ 3회, 충분성+재작성 LLM 호출 ≤ 5회, 파이프라인 시간 상한
  (워크스페이스 `deep_search_time_limit_ms`, 초과 시 부분 결과), 결과 총량 상한
  (`DEFAULT_DEEP_SEARCH_MAX_RESULTS`). 동일 쿼리 재검색 금지로 루프가 반드시 종료.
- **응답**: 기존 `SearchResponse`(results/sources_used/total/mode) + `agent` 메타데이터
  (`rounds/queries/graph_expanded/expansion_count/fallback/reason`). `mode="agent"`.
  확장 결과는 `SearchResult.expanded_from`으로 유래 표시(둘 다 하위호환 — 기존 검색은
  `agent:null`, 직접 결과는 `expanded_from:null`로 직렬화 생략).

## Search Logging (검색 로그 축적)

모든 검색(기존·agent)이 워크스페이스별로 로그에 남아 Phase 5 거버넌스 신호가 된다.

- **SearchLogStore** (`storage/search_log.rs`): `workspaces/{id}/search_logs/{YYYY-MM-DD}.jsonl`
  일 단위 append(무한 성장 방지·롤업 용이). 레코드: 시각·워크스페이스·쿼리·모드·결과 수·
  최고 점수·zero-result 여부·소요 시간·(agent 모드 시) 라운드 수.
- **실패 무해**: `append_best_effort`가 기록 실패를 삼킨다(warn만). 로그 장애가 검색을
  실패시키지 않는다. 파생 지표(결과 수·최고 점수·zero-result)는 순수 함수(`derive_metrics`).
- API 핸들러(`api/search.rs`)가 검색 소요 시간을 측정해 응답 성공 시 best-effort로 축적.

## Connectors (유입 파이프라인)

소유자의 지식이 사는 로컬 마크다운 생태계에서 Maia로 정보가 스스로 흘러들어오게 하는 유입
계층(Phase 4). 모든 유입은 소스 식별자 기반 중복 방지를 거쳐 신규/업데이트로 분기된다.

```
스케줄러(주기) ─┐
               ├─▶ ConnectorRunner.run_sync ─▶ Connector.fetch_changes(cursor) ─▶ [변경 항목]
API 트리거(수동)┘        │                                                          │
                        │  동시성 제한(buffer_unordered) + 항목별 태스크 격리        ▼
                        └─▶ ConnectorIngest(=Indexer).ingest_item ──▶ 소스 dedup 분기
                                                                       ├ 없음   → 신규(Created)
                                                                       ├ 변경   → 업데이트(Updated)
                                                                       └ 무변경 → 스킵(Skipped)
```

- **커넥터 공통 계약** (`connectors/mod.rs`): `Connector` trait = 증분 변경분 조회
  (`fetch_changes(cursor) -> FetchResult{items, next_cursor}`) + 소스 타입. 커서 의미는
  커넥터가 온전히 소유한다(불투명 문자열). 새 타입은 trait 구현 + `build_connector` 팩토리에
  한 줄 추가로 열린다(개방-폐쇄). 등록 스키마(`ConnectorInstance`/`ConnectorSpec`)는
  `workspace` 모듈이 소유해 `WorkspaceConfig.connectors`에 저장된다.
- **로컬 디렉토리 커넥터** (`connectors/local_dir.rs`): 등록 디렉토리를 명시적 스택으로 순회
  (async 재귀 회피)하며 수정 시각 > 커서인 파일만 유입. 확장자 필터·glob 제외 패턴·크기 상한
  스킵. **심볼릭 링크 방어**: 등록 루트는 canonicalize로 따르되 순회 중 발견된 링크는 따르지
  않는다(등록 범위 밖 읽기 차단). 깨진 인코딩·읽기 실패 파일은 스킵·기록(장애 격리). 읽기 전용.
- **유입 실행** (`Indexer: ConnectorIngest`): 저장은 기존 인덱싱 파이프라인을 재사용한다.
  `(source_type, source_id)` 일치 문서를 `DocumentStore::find_by_source`로 찾아
  — 없으면 신규(파싱+임베딩+출처 각인+그래프 자동 연결), 원본이 더 새로우면 내용 교체 업데이트
  (edges·created_at 보존, 출처 갱신), 더 새롭지 않으면 스킵. **1파일=1문서** 매핑으로 재유입 시
  문서 난립을 원천 차단한다(분할 위임 대신 업데이트 경로 결정성을 택함). 모드: `Parsed`(LLM 파싱,
  품질) / `Raw`(폴백 요약, rate limit 보호·대량 적재 우선).
- **동기화 상태·커서** (`connectors/sync_state.rs`): `workspaces/{ws}/connectors/{id}.json`에
  마지막 실행 시각·커서·결과 요약(처리/신규/갱신/스킵/실패 + 실패 목록 상한)을 영속화.
- **러너** (`connectors/runner.rs`): 동기화·대량 적재를 공유(대량 적재 = 커서 무시 `full` 동기화).
  `buffer_unordered`로 동시성 제한(LLM rate limit 보호), 항목별 `tokio::spawn`으로 패닉까지
  격리, 인메모리 진행 상태를 항목 완료마다 갱신(상태 API 실시간 관측). **커서는 실패 0일 때만
  전진** — 중단(상태 미저장)·부분 실패 시 다음 실행이 재스캔하고 이미 유입된 항목은 소스 dedup
  으로 스킵되어 이어서 처리된다(중단 재개·정보 유실 0).
- **스케줄러** (`connectors/scheduler.rs`): 고정 틱(기본 30s)마다 모든 워크스페이스의 활성
  커넥터를 훑어 `마지막 실행 + 주기 <= now`인 것을 실행. 각 실행을 태스크로 격리해 실패·패닉이
  루프나 서버를 죽이지 않는다. 기동 시 첫 틱 즉시 발생(자동 시작). 설정 변경은 다음 틱에 반영.
- **출처 메타데이터** (`Document.source`): `{source_type, source_id, modified_at, connector_id}`.
  raw JSON(SSoT)에 저장돼 reindex에서 살아남고, `GET /documents/{id}`로 출처를 확인한다.
  `#[serde(default)]`로 기존 문서와 하위호환.
- **API** (`api/connectors.rs`): 목록 `GET /api/connectors`, 상태 `GET .../:id/status`
  (워크스페이스 접근), 등록 `POST /api/connectors`, 삭제 `DELETE .../:id`, 즉시 실행
  `POST .../:id/sync`(admin). 즉시 실행은 백그라운드 태스크로 spawn해 요청 경로와 격리(202 반환).

## Patrol (자기 관리 & 메모리 거버넌스)

두뇌가 스스로의 기억 상태를 점검하는 **반자율** 계층(Phase 5). 완전 자동 수정의 자기참조
순환·오탐 피로를 피하려, 시스템은 후보를 식별해 플래그를 세우고(Review Queue) 소유자가
판단하며 그 피드백이 축적된다. **Patrol 자체는 읽기 + 플래그 + 감쇠 재계산만 하고 문서
내용을 변경·삭제하지 않는다** — 삭제는 오직 사람 판단(judge)에서만 일어난다.

```
스케줄러(주기) ─┐
               ├─▶ Patrol.run(ws) ─▶ 신호 수집(문서·피드백·freshness·소스 mtime)
API 트리거(수동)┘        │              │
                        │              ▼ 탐지기 4종(순수·격리 실행)
                        │        [staleness·중복·고아·외부 불일치] → 후보
                        │              ▼
                        │        Review Queue enqueue(열린 항목 dedup + 유형별 상한)
                        │              ▼
                        ├─▶ 엣지 시간 감쇠 자동 재계산(수학적 유지보수)
                        ├─▶ 메트릭 일 롤업(검색/그래프/유입/Patrol)
                        └─▶ 실행 이력 기록
```

- **탐지기** (`patrol/detectors.rs`): (a) staleness — freshness 기준점 나이 + "관련 없음"
  피드백 가중, (b) 중복 — 요약 토큰 Jaccard 유사도 상위 쌍(임베딩 없이 순수·결정적, 실제
  중복의 지배 사례인 동일 내용 재유입을 잡음), (c) 고아 — 엣지 없는 문서(신규 유예), (d)
  외부 불일치 — 커넥터 소스 수정 시각 vs 문서 유입 시각. **LLM 없이 수치 신호 기반**이라
  전부 순수 함수·단위 테스트. 임계값은 워크스페이스 `patrol.strictness`에서 파생(엄격할수록
  더 많이 플래그), 기본은 보수적(큐 쓰레기장 방지). 탐지기는 독립 실행되며 `combine`이 실패를
  격리한다(하나의 실패가 나머지를 막지 않음).
- **엣지 시간 감쇠** (`patrol/decay.rs`): `exp(-lambda * age_days)`로 오래된 엣지 가중치를
  낮춰 그래프 확장에서 뒤로 밀리게 한다(Phase 2 lambda 재사용). `Edge.base_weight`(생성 시점
  원본)를 기준으로 매번 재계산하므로 **반복 실행이 멱등**이다(base 없으면 최초 감쇠 때 현재
  weight로 고정 — 구버전 하위호환). 감쇠는 문서 내용 변경이 아니므로 `updated_at`을 건드리지
  않는다(staleness 기준점 리셋 방지). raw JSON이 SSoT, payload는 best-effort 동기화. 실행 시
  전 문서를 스냅샷으로 적재하지 않고 **문서별로 `write_lock` 아래 최신 상태를 재로드**해 감쇠하므로,
  패스가 도는 동안 동시 추가된 엣지를 stale 스냅샷으로 덮어쓰지 않는다(lost-update 제거).
- **Review Queue** (`patrol/review.rs`): 워크스페이스별 단일 JSON. 상태 대기→판단(유효/수정
  필요/삭제/기각). **열린 동일 (문서, 유형) 항목은 중복 생성 금지**, **판단은 멱등**(같은 판단
  재제출이 상태를 깨지 않음, 부수효과 이중 실행 없음). enqueue는 유형별 상한으로 폭주를 막는다.
- **판단 조율** (`Patrol.judge`): 유효 → freshness 기준점 갱신(당분간 staleness 유예), 삭제 →
  **복구 가능 삭제**(`Indexer.soft_delete_document`: 버전 보관 후 삭제, 멱등). 부수효과를 상태
  전이 전에 수행해 크래시 후 재제출로 복구되게 한다.
- **freshness** (`patrol/freshness.rs`): "유효" 판단 시각을 문서별 맵으로 별도 파일에 둔다
  (문서 raw JSON 미변경 — Patrol 불변식 순수 준수). staleness 탐지기가 이 기준점으로 나이를 잰다.
- **피드백** (`patrol/feedback.rs`): 검색 결과 "관련 없음"을 일 단위 JSONL로 축적(실패 무해)
  하고 문서별로 집계해 staleness 신호로 쓴다. 저장·집계까지만(ML 학습은 범위 밖).
- **메트릭** (`patrol/metrics.rs`): 일자 롤업 — 검색(횟수·zero-result율·평균 점수, search_log
  재활용), 그래프(노드·엣지·고아·평균 degree), 유입(문서 수·커넥터 요약 전략 분포), Patrol
  (탐지 수·큐 처리율). 계산은 순수 함수, 저장은 `{YYYY-MM-DD}.json`, 기간 조회 API.
- **스케줄러·이력** (`patrol/scheduler.rs`, `patrol/history.rs`): 커넥터 스케줄러와 동일한 틱
  루프 + 오류 격리. `patrol.frequency`(hourly/daily/weekly)를 주기로 환산해 마지막 실행(이력)
  기준으로 due를 판정한다. 수동 트리거는 동기 실행 후 리포트 반환.
- **mock 주입** (`PatrolExecutor` trait): 문서 실행기(전 문서 조회·삭제·감쇠)를 trait으로
  추상화해 `Indexer`가 구현하고, 오케스트레이터 단위 테스트는 mock으로 Qdrant·LLM 없이 전
  경로(탐지·enqueue·판단·감쇠·롤업)를 고정한다.
- **API** (`api/patrol.rs`): 실행 `POST /api/patrol/run`, 이력 `GET /api/patrol/history`, 큐
  `GET /api/review`(상태·유형 필터)·판단 `POST /api/review/judge`(단건·일괄 통합), 피드백
  `POST /api/feedback`, 메트릭 `GET /api/metrics`(기간). 실행·판단·피드백=write, 조회=워크스페이스 접근.

## Versioning (버전 보관)

업데이트 경로에서 이전 문서 상태를 보관한다(잘못된 업데이트의 안전망).

- **VersionStore** (`storage/versions.rs`): `workspaces/{id}/versions/{doc_id}/{millis}.json`.
  스냅샷은 문서 전체(엣지 포함) + `archived_at`. 동일 밀리초 충돌 시 uuid suffix.
- `update_in_workspace`는 덮어쓰기 **전에** archive하고, archive 실패 시 업데이트를 중단한다
  ("이전 버전 보장" 시맨틱). 복원 UI는 백로그.

## Time-Aware Search (시간 인식 검색)

- **시간 감쇠** (opt-in, `time_decay=true`): `exp(-lambda * age_days)`로 재정렬. 표시 점수는
  원본 유사도를 유지하고 **순위만** 조정 → "동일 유사도면 최신 우선". lambda는 워크스페이스
  설정 `time_decay_lambda`.
- **기간 필터**: `since`/`until`로 created_at 범위 필터.
- **처리 순서**: 기간 필터 → 관련성 필터(원본 cosine) → 감쇠 재정렬. 오래됐지만 관련있는
  문서가 임계값에서 사라지지 않는다. 순수 함수(`now` 파라미터)로 단위 테스트.

## Search Modes

| Mode | 방식 | 특징 |
|------|------|------|
| **Hybrid** (기본) | Vector + Keyword → RRF 결합 | 의미 + 키워드 동시 고려, 가장 정확 |
| **Vector (Semantic)** | 쿼리 임베딩 → Qdrant 코사인 유사도 | 의미가 비슷한 문서 매칭 |
| **Keyword (BM25)** | 전체 문서 로드 → BM25 스코어링 | 정확한 단어 일치 기반 |

위 세 모드는 단일 라운드다. `agent:true`는 이 hybrid 검색을 실행기로 삼아 다중 라운드
+ 그래프 확장으로 능동 회상한다 → [Search Agent (검색 회상)](#search-agent-검색-회상) 참조.

## Port Assignments

| Service | Port |
|---------|------|
| Backend (HTTP API + Static) | 8080 |
| Qdrant (REST) | 6333 |
| Qdrant (gRPC) | 6334 |

## Data Portability

서버 이전 시 `backend/data/workspaces/` 디렉토리(원본 문서 + 워크스페이스 설정 + 버전 스냅샷
+ 거버넌스 데이터: Review Queue·freshness·피드백·메트릭·Patrol 이력)와 `api_keys.json`을 복사
후 워크스페이스별 `POST /api/reindex?workspace={id}` 호출.
Qdrant는 파생 인덱스이므로 reindex만으로 전체 상태(**그래프 엣지 포함** — raw JSON의 edges가
summary chunk payload로 복원됨) 복원 가능. 설정(`settings.json`)은 환경마다 재설정.
