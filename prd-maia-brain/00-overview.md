# Maia — 개인 인공두뇌 완성 PRD

> **Version:** 1.0
> **Date:** 2026-07-06
> **Status:** Active
> **Author:** 관리 에이전트 (Daybreak312 위임)

---

## Repository & Branching

| Item | Value |
|------|-------|
| **Repository** | `/Users/daybreak312/IdeaProjects/project-maia` (로컬 전용, 리모트 없음) |
| **Base branch** | `main` |
| **Work branch pattern** | `dev/phase{N}-{slug}` (오케스트레이터가 생성) |
| **Merge strategy** | phase 완료 후 관리 에이전트가 main으로 머지 |
| **Code location** | 모노레포: `backend/`(Rust), `mcp/`(TypeScript), `frontend/`(React) |

## Background (이 PRD가 존재하는 이유)

Maia는 Daybreak312의 **개인 인공두뇌**다. 소유자의 모든 정보(일상 기록, 프로젝트, 관심사, 의사결정)를 저장하는 원장이자, 소유자의 AI 에이전트들(OpenClaw, Claude Code 등)이 공유하는 문맥 엔진이 되는 것이 최종 목표다.

Phase 1~2(기본 RAG + 원자적 사실 청킹)는 완성되어 로컬 Docker로 가동 중이다. Phase 3 설계(`contexts/ideas_agent_layer.md`, `contexts/phase3_tasks.md`)가 문서화되어 있고, 워크스페이스 시스템의 스토리지 레이어까지 구현된 상태에서 중단됐다. 이 PRD는 그 설계를 이어받아 **끝까지 완성**하기 위한 실행 사양이다.

방향성 (`contexts/maia_direction_alignment.md` 결론):
- Maia는 "문서를 잘 찾는 RAG 앱"이 아니라 **"에이전트가 사람처럼 일하도록 문맥을 제공하는 메모리 레이어"**다.
- 검색은 수단이고, 목적은 **살아있는 지식 그래프**다.
- 유지보수(Patrol)는 청소가 아니라 **memory governance**다.

## Problem

1. 워크스페이스 리팩토링이 절반에서 멈춤 — 스토리지는 workspace-aware인데 API/인증/MCP가 연결 안 됨. BM25 단위 테스트 2개가 깨진 채 방치.
2. 저장이 수동적 — 들어온 정보를 그대로 저장할 뿐, 기존 지식과 비교·병합·분할하는 판단이 없다.
3. 문서가 벡터 공간에 고립 — 문서 간 관계(엣지)가 없어 연관 지식 탐색 불가.
4. 검색이 1회성 — 결과가 부족해도 스스로 재탐색하지 않는다.
5. 유입 마찰 — 소유자의 실제 지식(OpenClaw 메모리, 데일리 노트, 크론 리포트)이 Maia 밖에 쌓이고 있다.
6. 자기 관리 부재 — 오래된 정보, 중복, 고아 문서를 시스템이 인지하지 못한다.

## Solution

```
                         ┌──────────────────────────────────────┐
  Claude Code / OpenClaw │              Maia Backend            │
  (MCP: search/ingest/   │                                      │
   deep_search/neighbors)│  ┌─────────┐   ┌──────────────────┐  │
        │                │  │ Search  │   │ Ingest Agent     │  │
        ▼                │  │ Agent   │   │ (분할/병합/연결) │  │
   MCP Server ──HTTP──▶  │  └────┬────┘   └────────┬─────────┘  │
                         │       ▼                 ▼            │
   Connectors ─────────▶ │  Hybrid Search ──▶ Knowledge Graph   │
   (로컬 마크다운 등)    │  (Vector+BM25+RRF)  (Docs + Edges)   │
                         │       │                 │            │
   Patrol Agent ───────▶ │       ▼                 ▼            │
   (거버넌스/리뷰 큐)    │    Qdrant (워크스페이스별 컬렉션)    │
                         │    raw JSON (Single Source of Truth) │
                         └──────────────────────────────────────┘
```

## Key Decisions

| Decision | Rationale |
|----------|-----------|
| raw 문서 JSON(DocumentStore)이 유일한 Single Source of Truth | Qdrant는 파생 인덱스. reindex만으로 전체 상태(엣지 포함) 복원 가능해야 함 — 데이터 이식성 원칙 유지 |
| 그래프 엣지는 Document(raw JSON)에 저장하고 Qdrant payload로 비정규화 | 신규 DB 도입 없이 기존 스택 유지. 엣지가 reindex에서 살아남는 유일한 구조 |
| 워크스페이스 격리 = Qdrant 컬렉션 분리 (`documents_{id}`) | 이미 구현됨. 격리가 깔끔하고 삭제가 원자적 |
| 인증 = API Key 기반 워크스페이스 스코핑 (RBAC 없음) | 개인+소규모 공유 용도에 충분. `MAIA_API_KEY` 마스터키 하위호환 |
| 에이전트 판단은 기존 `LlmProvider` trait 경유 | provider 교체 가능성 유지, 테스트에서 mock 주입 가능 |
| **판단 실패 시 폴백 필수**: smart_ingest 실패 → raw 저장, agent 검색 실패 → 기존 hybrid 결과 | 인공두뇌 제1원칙: 덜 똑똑한 건 괜찮지만 기억을 잃으면 안 된다 |
| 단위 테스트 최우선 (소유자 명시 요구) | 향후 vibe-coding 가드레일. LLM/Qdrant 의존 없는 순수 로직은 전부 단위 테스트 |

## Anti-Goals (전체 범위 밖)

- **멀티유저 RBAC / 조직 계정 체계** — API Key 스코핑으로 충분. (이유: 개인 시스템, 과잉 설계 방지)
- **신규 데이터베이스 도입 (SQLite, Neo4j, Postgres 등)** — Qdrant + JSON 파일로 해결. (이유: 운영 부담 최소화, 이식성)
- **클라우드 배포/CI 파이프라인** — 로컬 Docker 운영. 배포 자동화는 별도 작업.
- **그래프 시각화 페이지(D3/Cytoscape)** — 백로그. 두뇌 기능과 무관한 데모성 기능.
- **Notion/GitHub/Readwise 등 외부 SaaS 커넥터** — 백로그. 프레임워크만 갖추고 로컬 디렉토리 커넥터를 레퍼런스로 구현.
- **프론트엔드 디자인 폴리시** — 기능 동작 수준이면 충분. 스타일링에 시간 쓰지 않는다.
- **실제 LLM API를 호출하는 자동 테스트** — 비용/비결정성. mock으로 검증.

## Glossary

| 용어 | 정의 |
|------|------|
| **Document** | 지식의 기본 단위. summary + facts + tags + entities + edges를 가진 raw JSON |
| **Fact chunk** | LLM이 추출한 독립적 사실 단위. 문서당 N개, 각각 임베딩됨 |
| **Summary chunk** | 문서당 1개인 요약 청크. BM25 검색과 엣지 payload의 저장 위치 |
| **Workspace** | 격리된 지식 공간 (예: personal, work). 컬렉션/설정/키 스코프의 단위 |
| **Edge** | 문서 간 방향성 관계. relation_type + weight + created_at |
| **Ingest Agent** | 저장 전에 신규/업데이트/분할/중복을 판단하는 LLM 에이전트 |
| **Search Agent** | 결과 충분성을 평가하며 재검색·그래프 확장하는 LLM 에이전트 |
| **Patrol** | 주기 실행되는 유지보수 에이전트. 수정하지 않고 플래그만 세움 |
| **Review Queue** | Patrol이 세운 플래그를 사람이 판단하는 대기열 |
| **Connector** | 외부 소스에서 정보를 주기적으로 끌어오는 유입 모듈 |

## Constraints (전 Phase 공통)

- `cargo test`(backend), `npm run build`(mcp, frontend) 전부 통과가 모든 phase의 종료 조건. 커밋 전 실행.
- 기존 137개 테스트 중 실패로 남는 테스트가 없어야 한다 (2개 기존 실패는 Phase 1에서 수정).
- LLM 호출 로직은 `LlmProvider` trait 뒤에 두고, 테스트는 mock provider로. 실 API 호출 테스트 금지.
- Qdrant가 필요한 통합 테스트는 환경변수 가드(예: `MAIA_TEST_QDRANT_URL` 설정 시에만 실행) 또는 `#[ignore]`. 기본 `cargo test`는 외부 의존 없이 통과해야 함.
- API 하위호환: 기존 엔드포인트 시그니처를 깨지 않는다. 워크스페이스 미지정 시 `default` 워크스페이스로 동작 (기존 클라이언트 무중단).
- 에러는 침묵하지 않는다: 에이전트 판단 실패·폴백 발생 시 응답 메타데이터와 로그에 명시.
- 이 호스트(macOS)에는 GNU `timeout`이 없다. 스크립트에서 사용 금지.
- 커밋 메시지는 한국어로, 의미 단위로 잘게.

## Technical Context

### Existing Patterns (관찰된 사실)

- **Rust backend**: axum 0.7, tokio. `AppState`에 의존성 주입. 에러는 `anyhow::Result` + HTTP 매핑.
- **저장**: `DocumentStore`(raw JSON, 워크스페이스 경로 인식 완료), `QdrantStorage`(워크스페이스별 컬렉션 `documents_{id}` 구현 완료, `ensure_collection`/`recreate_collection`/`delete_by_document_id` 등 존재).
- **워크스페이스**: `workspace/config.rs`(patrol/parsing/search/connectors 설정 필드), `workspace/manager.rs`(CRUD, personal/enterprise 프리셋, default 보호, 레거시 마이그레이션) — **테스트 37개 통과, 단 API에 미연결**.
- **인증**: `auth/keys.rs`(ApiKey 모델 + ApiKeyManager, SHA-256 해시, 워크스페이스 접근 체크) — **구현+테스트 존재, 단 미들웨어 미연결**. 구 `auth.rs`는 삭제됨.
- **검색**: `core/search.rs` BM25 + RRF 하이브리드. **주의: `test_tokenize_mixed`, `test_bm25_scoring` 2개 실패 중** — 토크나이저가 영문 토큰("react")을 놓치는 회귀.
- **인덱싱**: 1 Document → 1 summary chunk + M fact chunks. payload: `document_id, chunk_type, chunk_index, chunk_text, summary, created_at`. payload index: `document_id`(Keyword), `chunk_type`(Keyword).
- **MCP**(TypeScript): `mcp/src/index.ts` 단일 서버, `maia-client.ts` REST 클라이언트. Tools: `search_context`, `ingest_information`, `get_document`, `list_recent_documents`.
- **Frontend**: React+Vite, pages(AddPage/SearchPage/BrowsePage/AdminPage), `api/client.ts` 중앙 클라이언트.
- **테스트 컨벤션**: 각 모듈 하단 `#[cfg(test)] mod tests`, tempdir 기반 파일 테스트. 현재 137개(37 lib + 100 bin).

### Integration Points

- API 레이어(`api/mod.rs`)가 워크스페이스/키 시스템과 만나는 접합면이 Phase 1의 핵심.
- `core/indexer.rs`가 IngestAgent(Phase 2)의 하위 실행기가 된다 — 에이전트는 판단만 하고, 실제 저장은 기존 인덱싱 파이프라인 재사용.
- MCP 서버는 REST API의 얇은 브릿지 — 백엔드에 기능이 생기면 MCP tool로 노출하는 패턴 유지.
- 운영 환경: 로컬 Docker(`project-maia-app-1` :9080, `project-maia-qdrant-1` 내부 네트워크). 개발 중 재배포는 범위 밖, 완성 후 관리 에이전트가 수행.

### Conventions

- 한국어 주석/커밋, 코드 식별자는 영어.
- 파일 기반 설정(JSON) 선호: `data/workspaces/{id}/config.json`, `api_keys.json` 패턴 유지.
- 프로젝트 컨텍스트 문서는 `contexts/`가 SSoT — 아키텍처 변경 시 `contexts/architecture.md` 갱신.

## Phases

| Phase | Goal | Document |
|-------|------|----------|
| 1 | 워크스페이스 시스템 완성 + 회귀 제거 (기반 안정화) | `01-phase1-foundation.md` |
| 2 | Ingest Agent + 지식 그래프 + 시간 감쇠 (두뇌 코어) | `02-phase2-brain-core.md` |
| 3 | Search Agent (검색 지능) | `03-phase3-search-agent.md` |
| 4 | 커넥터 프레임워크 + 로컬 지식 대량 유입 (연결) | `04-phase4-connectors.md` |
| 5 | Patrol + Review Queue + 메트릭 (자기 관리) | `05-phase5-patrol.md` |
