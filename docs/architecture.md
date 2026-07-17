# 시스템 아키텍처

> 최종 검증: 2026-07-17 · 기준 커밋: 712484e · [문서 인덱스](README.md)

## 토폴로지

```
┌─ 클라이언트 머신 ──────────────────────────┐      ┌─ 서버 (docker compose) ────────────┐
│                                             │      │                                     │
│  Claude Code / OpenClaw / Claude Desktop 등 │      │  Maia Backend (Rust, :8080)         │
│       │                                     │      │   ├─ REST API (Bearer 인증)         │
│       ▼                                     │      │   ├─ 정적 서빙 (frontend 빌드)      │
│  MCP Server (STDIO, Node.js) ── HTTP ───────┼──────▶   ├─ 커넥터·Patrol 스케줄러         │
│                                             │      │   └─ /knowledge (ro 마운트) 스캔    │
│  브라우저 (관리 UI) ── HTTPS/터널 ──────────┼──────▶       │                             │
│                                             │      │       ▼                             │
└─────────────────────────────────────────────┘      │  Qdrant (:6333, 내부 네트워크 전용) │
                                                     └─────────────────────────────────────┘
```

- 백엔드와 Qdrant는 docker 내부 네트워크로만 통신하고, 호스트에는 백엔드 포트만
  (구성에 따라 `127.0.0.1:9080`) 바인딩된다.
- 외부 접근은 Cloudflare Tunnel 또는 SSH 터널을 앞단에 둔다. 구성별 차이는
  [deployment.md](deployment.md) 참조.

## 모노레포 구조

```
project-maia/
├── backend/                 # Rust axum 서버 (원장 + 인덱스 + API)
│   ├── src/
│   │   ├── main.rs          # 진입점: AppState 조립, 라우터, 스케줄러 기동
│   │   ├── config.rs        # 환경변수 (SERVER_PORT, QDRANT_URL, DATA_DIR, MAIA_API_KEY, STATIC_DIR)
│   │   ├── settings.rs      # 런타임 설정 (LLM 키·provider 선택, settings.json)
│   │   ├── auth/            # Bearer 인증 미들웨어, API 키 발급·검증 (SHA-256)
│   │   ├── workspace/       # 워크스페이스 CRUD·설정·커넥터 등록 스키마
│   │   ├── llm/             # Provider 추상화: gemini/claude/openai/codex/local
│   │   ├── api/             # HTTP 핸들러 (전 엔드포인트 워크스페이스 인식)
│   │   ├── core/            # indexer(오케스트레이션)·ingest_agent·search_agent·search(BM25/RRF)
│   │   ├── connectors/      # 유입 파이프라인: local_dir, runner, scheduler, sync_state
│   │   ├── patrol/          # 자기 관리: detectors, review, decay, freshness, feedback, metrics
│   │   ├── storage/         # qdrant, documents(raw JSON), versions, search_log
│   │   └── models/          # Document, Edge, Entity, API DTO
│   ├── static/              # 레거시 정적 프론트 (컨테이너에서는 미사용 — STATIC_DIR 참조)
│   ├── docker/onnx_compat.c # onnxruntime 링크 호환 심 (Dockerfile Stage 2)
│   └── .env.example
├── mcp/                     # MCP 브릿지 (TypeScript, STDIO) — tool 6종
├── frontend/                # React + Vite SPA (Add/Search/Browse/Review/Admin)
├── deploy/oracle/           # Oracle Cloud용 compose (CPU 캡·localhost 바인딩)
├── docker-compose.yml       # Cloudflare Tunnel 동봉 구성
├── docker-compose.local.yml # 로컬 실행 구성 (127.0.0.1:9080)
├── Dockerfile               # 3-stage 빌드 (frontend → rust → runtime)
├── docs/                    # ★ 사실 레퍼런스 문서 (이 폴더)
├── contexts/                # 에이전트 작업 컨텍스트 (스펙·플랜·결정 로그)
└── prd-maia-brain/          # Phase 1~6 PRD (역사 기록)
```

## 기술 스택

| Layer | Technology | 선정 이유 |
|-------|------------|-----------|
| Backend | **Rust + axum 0.7** | 단일 바이너리, 성능, 타입 안전성 |
| Vector DB | **Qdrant** | Rust 네이티브 클라이언트, payload 필터링 |
| 파싱 LLM | Gemini / Claude(API키·구독 OAuth) / OpenAI / **Codex(구독)** | Provider 패턴, 구독 기반 종량제 탈피 |
| 임베딩 | Gemini(3072d) / OpenAI(1536d) / **Local fastembed(384d)** | 로컬 임베딩으로 외부 키 없이 자립 |
| MCP | **TypeScript SDK** | MCP 생태계 성숙도, STDIO transport |
| Frontend | **React 19 + Vite** | SPA, 백엔드 정적 서빙과 상대 경로 통신 |
| Container | **Docker (debian trixie 베이스)** | onnxruntime glibc 2.38+ 요구 → trixie 필수 |

## 백엔드 기동 순서

`backend/src/main.rs` 기준.

1. 로깅(`tracing`, 기본 `maia=info`) → 환경변수 로드(`Config::from_env`, dotenv 지원)
2. `SettingsManager` — `DATA_DIR/settings.json` 로드
3. `WorkspaceManager` — `default` 워크스페이스 보장 + 레거시(`data/raw/`) 마이그레이션
4. `ApiKeyManager` — `DATA_DIR/api_keys.json` 로드 (손상 시 `.corrupt` 백업 후 degrade)
5. 스토리지 — Qdrant 클라이언트, DocumentStore, VersionStore, SearchLogStore, SyncStateStore
6. `Indexer` (유입·검색 오케스트레이터) + `ConnectorRunner`
7. **커넥터 스케줄러** 기동 (백그라운드, 기본 틱 30초)
8. **Patrol 스케줄러** 기동 (백그라운드)
9. 라우터 조립: 인증 미들웨어(`require_api_key`) + CORS(전체 허용) + TraceLayer
   + 정적 파일 fallback(`STATIC_DIR`) → `0.0.0.0:{SERVER_PORT}` 리슨

## 요청 처리 흐름

```
요청 → require_api_key 미들웨어 (마스터키 → 등록 키 → 401)
     → AuthContext 주입 (권한·워크스페이스 스코프)
     → 핸들러: resolve_and_authorize_workspace (?workspace= 해석, 404/403)
     → Indexer / 저장소 호출
     → 응답 (실패 시 원칙: 유입은 raw 폴백, 검색은 폴백 결과 + fallback 표시)
```

인증·권한의 상세 규칙은 [api.md](api.md), 데이터 흐름의 세부는
[ingest.md](ingest.md) / [search.md](search.md) 참조.

## 관련 문서

- 데이터 구조와 디스크 레이아웃 — [data-model.md](data-model.md)
- LLM 프로바이더 계층 — [llm-providers.md](llm-providers.md)
- 배포 구성 3종 비교 — [deployment.md](deployment.md)
