# System Architecture

## Tech Stack

| Layer | Technology | Rationale |
|-------|------------|-----------|
| Language | **Rust** | 단일 바이너리, 성능, 타입 안전성 |
| HTTP Server | **axum** | 현대적, tokio 기반, 간결함 |
| Vector DB | **Qdrant** | Rust 네이티브, 필터링 강력, Docker 지원 |
| LLM | **Gemini/Claude/OpenAI** | 추상화된 Provider 패턴 |
| Frontend | **Vanilla HTML/CSS/JS** | MVP, 의존성 최소화 |
| Container | **Docker** | 이식성, 볼륨으로 데이터 분리 |

## Directory Structure

```
contextforge/
├── Cargo.toml
├── src/
│   ├── main.rs                 # 진입점, axum 서버, AppState
│   ├── config.rs               # 환경설정
│   │
│   ├── llm/                    # AI Provider 추상화 레이어
│   │   ├── mod.rs              # LlmProvider, EmbeddingProvider traits
│   │   ├── gemini.rs           # Gemini 구현
│   │   ├── claude.rs           # Claude 구현
│   │   └── openai.rs           # OpenAI 구현 (GPT + Embeddings)
│   │
│   ├── api/                    # HTTP API 레이어
│   │   ├── mod.rs
│   │   ├── ingest.rs           # POST /ingest
│   │   ├── search.rs           # POST /search
│   │   ├── documents.rs        # GET /documents
│   │   └── settings.rs         # 설정 API
│   │
│   ├── core/                   # 비즈니스 로직
│   │   ├── mod.rs
│   │   └── indexer.rs          # 인덱싱 오케스트레이션
│   │
│   ├── storage/                # 데이터 레이어
│   │   ├── mod.rs
│   │   ├── qdrant.rs           # Qdrant 클라이언트
│   │   └── documents.rs        # 원본 문서 저장 (파일)
│   │
│   ├── settings.rs             # 설정 관리 (API Key 등)
│   │
│   └── models/                 # 데이터 구조
│       ├── mod.rs
│       └── document.rs         # Document, Entity, Tag
│
├── static/                     # 프론트엔드 정적 파일
│   ├── index.html              # 메인 (정보 추가)
│   ├── search.html             # 검색
│   ├── admin.html              # 설정 관리
│   ├── style.css
│   └── app.js
│
├── data/                       # 런타임 데이터 (Volume)
│   ├── raw/                    # 원본 문서
│   ├── settings.json           # API Key 및 설정
│   └── qdrant/                 # Qdrant 데이터
│
├── contexts/                   # 프로젝트 컨텍스트 문서
├── Dockerfile
├── docker-compose.yml
└── .env.example
```

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
│  GeminiEmbedding  │  OpenAiEmbedding                        │
│  (768 dim)        │  (1536 dim)                             │
└─────────────────────────────────────────────────────────────┘
```

## Data Flow

```
[사용자 입력]
   │
   ▼
┌──────────────────────────────────────────────────────────┐
│  POST /ingest                                            │
│    body: { "content": "자연어 텍스트" }                  │
└──────────────────────────────────────────────────────────┘
   │
   ▼
┌──────────────────────────────────────────────────────────┐
│  Indexer::ingest()                                       │
│    1. SettingsManager에서 현재 provider 조회             │
│    2. LlmProvider::parse() 호출                          │
│       → summary, tags, entities 추출                     │
│    3. EmbeddingProvider::embed() 호출                    │
│       → summary → vector                                 │
│    4. DocumentStore에 원본 저장                          │
│    5. QdrantStorage에 벡터 저장                          │
└──────────────────────────────────────────────────────────┘
   │
   ▼
[응답]
   { "id": "xxx", "summary": "...", "tags": [...] }
```

## Settings Storage

```json
// data/settings.json
{
  "api_keys": {
    "gemini": "AIza...",
    "claude": "sk-ant-...",
    "openai": "sk-..."
  },
  "parsing_provider": "gemini",
  "embedding_provider": "gemini"
}
```

## Port Assignments

| Service | Port |
|---------|------|
| App (HTTP API + Static) | 8080 |
| Qdrant (REST) | 6333 |
| Qdrant (gRPC) | 6334 |

## Embedding Dimensions by Provider

| Provider | Model | Dimension |
|----------|-------|-----------|
| Gemini | text-embedding-004 | 768 |
| OpenAI | text-embedding-3-small | 1536 |

**Note**: Qdrant collection은 첫 문서 저장 시 해당 임베딩 차원으로 생성됨.
Provider 변경 시 차원이 다르면 새 collection 필요.
