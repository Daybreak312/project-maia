# System Architecture

## Monorepo Structure

```
project-maia/
├── backend/                    # Rust RAG 서버
│   ├── Cargo.toml
│   ├── src/
│   │   ├── main.rs             # 진입점, axum 서버, AppState
│   │   ├── config.rs           # 환경설정 (SERVER_PORT, QDRANT_URL, DATA_DIR, MAIA_API_KEY)
│   │   ├── auth.rs             # API 키 인증 미들웨어
│   │   ├── settings.rs         # 설정 관리 (LLM API Key, Provider 선택)
│   │   │
│   │   ├── llm/                # AI Provider 추상화 레이어
│   │   │   ├── mod.rs          # LlmProvider, EmbeddingProvider traits
│   │   │   ├── gemini.rs       # Gemini 구현
│   │   │   ├── claude.rs       # Claude 구현
│   │   │   └── openai.rs       # OpenAI 구현
│   │   │
│   │   ├── api/                # HTTP API 레이어
│   │   │   ├── mod.rs
│   │   │   ├── ingest.rs       # POST /ingest
│   │   │   ├── search.rs       # POST /search, GET /tags
│   │   │   ├── documents.rs    # CRUD /documents, GET /recent, POST /api/reindex
│   │   │   └── settings.rs     # 설정 API
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
│   │   ├── raw/                # 원본 문서 JSON (Single Source of Truth)
│   │   ├── settings.json       # LLM API Key 및 Provider 설정
│   │   └── qdrant/             # Qdrant 벡터 DB 데이터
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

## Authentication

- 환경변수 `MAIA_API_KEY` 설정 시 모든 API 엔드포인트에 `Authorization: Bearer <key>` 필수
- 미설정 시 인증 비활성화 (로컬 개발용)
- `/health` 엔드포인트는 항상 인증 불필요

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
| `get_tags` | 태그 목록 | `GET /tags` |

## Indexing Architecture (Atomic Fact Chunking)

```
1 Document → N Qdrant Points
├── 1 summary chunk (embed(summary))       ← 항상 존재
└── M fact chunks   (embed(fact[0..M-1]))  ← LLM이 추출한 독립적 사실

Point Payload: { document_id, chunk_type, chunk_index, chunk_text, summary, tags, created_at }
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

서버 이전 시 `backend/data/raw/` 디렉토리만 복사 후 `POST /api/reindex` 호출.
설정(`settings.json`)은 환경마다 재설정.
