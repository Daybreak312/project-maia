# System Architecture

## Monorepo Structure

```
project-maia/
├── backend/                    # Rust RAG 서버
│   ├── Cargo.toml
│   ├── src/
│   │   ├── main.rs             # 진입점, axum 서버, AppState(indexer/settings/workspaces/api_keys)
│   │   ├── config.rs           # 환경설정 (SERVER_PORT, QDRANT_URL, DATA_DIR, MAIA_API_KEY)
│   │   ├── settings.rs         # 설정 관리 (LLM API Key, Provider 선택)
│   │   │
│   │   ├── auth/               # 인증/인가
│   │   │   ├── mod.rs          # require_api_key 미들웨어 (마스터키→ApiKeyManager→401, AuthContext 주입)
│   │   │   └── keys.rs         # ApiKey, ApiKeyManager, AuthContext, Permission (SHA-256 해시)
│   │   │
│   │   ├── workspace/          # 워크스페이스 시스템
│   │   │   ├── mod.rs
│   │   │   ├── config.rs       # WorkspaceConfig (patrol/parsing/search + cross_workspace), 템플릿
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
│   │   │   ├── ingest.rs       # POST /ingest?workspace=
│   │   │   ├── search.rs       # POST /search?workspace= (교차검색 결정)
│   │   │   ├── documents.rs    # CRUD /documents, GET /recent, POST /api/reindex (workspace 스코프)
│   │   │   ├── workspaces.rs   # 워크스페이스 CRUD API (admin 전용)
│   │   │   ├── keys.rs         # API 키 발급/조회/폐기 API (admin 전용)
│   │   │   └── settings.rs     # 설정 API (mutation은 admin)
│   │   │
│   │   ├── core/               # 비즈니스 로직
│   │   │   ├── mod.rs
│   │   │   ├── indexer.rs      # 인덱싱 오케스트레이션
│   │   │   └── search.rs       # BM25, RRF, 하이브리드 검색
│   │   │
│   │   ├── storage/            # 데이터 레이어
│   │   │   ├── mod.rs
│   │   │   ├── qdrant.rs       # Qdrant 벡터 검색/저장
│   │   │   └── documents.rs    # 원본 문서 파일 저장
│   │   │
│   │   └── models/
│   │       ├── mod.rs
│   │       └── document.rs     # Document, Entity, API DTO
│   │
│   ├── static/                 # 레거시 정적 프론트엔드 (Vanilla HTML)
│   ├── data/                   # 런타임 데이터 (Volume)
│   │   ├── workspaces/{id}/    # 워크스페이스별 격리 저장
│   │   │   ├── config.json     #   워크스페이스 설정
│   │   │   └── documents/      #   원본 문서 JSON (Single Source of Truth)
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
| LLM | **Gemini/Claude/OpenAI** | 추상화된 Provider 패턴으로 교체 가능 |
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

### Permission (3단계)
| 권한 | 문서 읽기 | 문서 쓰기 | 워크스페이스·키 관리 |
|------|----------|----------|---------------------|
| `read_only`  | ✅ | ❌ | ❌ |
| `read_write` | ✅ | ✅ | ❌ |
| `admin`      | ✅ | ✅ | ✅ |

### API Key 모델 (`data/api_keys.json`)
- 저장: `key_id`, `hashed_key`(SHA-256, 평문 미저장), `label`, `workspaces[]`, `permissions`, `created_at`, `last_used_at`, `expires_at?`
- 발급 API 응답에서만 평문 키 1회 노출. 목록 조회는 해시를 제외한 뷰 반환.

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
│  - parse(content) -> ParsedContent                          │
│  - validate_api_key() -> bool                               │
├─────────────────────────────────────────────────────────────┤
│  GeminiProvider  │  ClaudeProvider  │  OpenAiChatProvider   │
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│                   EmbeddingProvider (trait)                  │
│  - embed(text) -> Vec<f32>                                  │
│  - dimension() -> usize                                     │
├─────────────────────────────────────────────────────────────┤
│  GeminiEmbedding (3072 dim) │  OpenAiEmbedding (1536 dim)  │
└─────────────────────────────────────────────────────────────┘
```

## MCP Tools

| Tool | Description | Maia API |
|------|-------------|----------|
| `search_context` | 개인 지식 베이스 검색 | `POST /search` |
| `ingest_information` | 새 정보 저장 | `POST /ingest` |
| `get_document` | 문서 원문 조회 | `GET /documents/{id}` |
| `list_recent_documents` | 최근 문서 목록 | `GET /recent` |

모든 tool은 선택적 `workspace` 인자를 받는다. 미지정 시 `MAIA_WORKSPACE` 환경변수,
그것도 없으면 서버가 API 키의 기본 워크스페이스를 사용한다.

## Indexing Architecture (Atomic Fact Chunking)

```
1 Document → N Qdrant Points
├── 1 summary chunk (embed(summary))       ← 항상 존재
└── M fact chunks   (embed(fact[0..M-1]))  ← LLM이 추출한 독립적 사실

Point Payload: { document_id, chunk_type, chunk_index, chunk_text, summary, created_at }
Payload Indexes: document_id (Keyword), chunk_type (Keyword)
```

**Vector 검색**: chunk 단위 검색 → `document_id` 기준 그룹핑 → 최고 점수 + matched_facts 수집
**Keyword 검색**: `chunk_type="summary"` chunk만 대상 → BM25 스코어링
**Hybrid 검색**: Vector + Keyword 결과를 RRF로 순서 결합, raw cosine similarity 점수 유지

**검색 품질 필터링**: 절대 임계값(0.5) + 점수 드롭 감지(0.15) + 최대 결과(5개)

## Search Modes

| Mode | 방식 | 특징 |
|------|------|------|
| **Hybrid** (기본) | Vector + Keyword → RRF 결합 | 의미 + 키워드 동시 고려, 가장 정확 |
| **Vector (Semantic)** | 쿼리 임베딩 → Qdrant 코사인 유사도 | 의미가 비슷한 문서 매칭 |
| **Keyword (BM25)** | 전체 문서 로드 → BM25 스코어링 | 정확한 단어 일치 기반 |

## Port Assignments

| Service | Port |
|---------|------|
| Backend (HTTP API + Static) | 8080 |
| Qdrant (REST) | 6333 |
| Qdrant (gRPC) | 6334 |

## Data Portability

서버 이전 시 `backend/data/workspaces/` 디렉토리(원본 문서 + 워크스페이스 설정)와
`api_keys.json`을 복사 후 워크스페이스별 `POST /api/reindex?workspace={id}` 호출.
Qdrant는 파생 인덱스이므로 reindex만으로 전체 상태(향후 그래프 엣지 포함) 복원 가능.
설정(`settings.json`)은 환경마다 재설정.
