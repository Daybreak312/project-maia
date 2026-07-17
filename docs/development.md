# 개발 가이드

> 최종 검증: 2026-07-17 · 기준 커밋: 712484e · [문서 인덱스](README.md)

## 요구사항

- Rust (stable, edition 2021) · Node.js 20+ · Docker (통합 실행·로컬 임베딩 검증용)
- 로컬 임베딩을 **네이티브로** 돌리려면 onnxruntime이 로드 가능해야 한다 — macOS는
  보통 문제없고, Linux는 glibc 2.38+ 필요. 애매하면 컨테이너로 검증한다
  (→ [deployment.md](deployment.md)).

## 실행

```bash
# 통합 (권장): 백엔드 + Qdrant + 프론트 빌드 서빙
cp .env.example .env
docker compose -f docker-compose.local.yml up -d --build   # http://127.0.0.1:9080

# 백엔드만 네이티브로
docker run -d -p 6333:6333 qdrant/qdrant     # Qdrant 먼저
cd backend && cargo run                       # 기본 :8080, ./data 사용

# 프론트 개발 서버 / MCP 워치 빌드
cd frontend && npm ci && npm run dev
cd mcp && npm ci && npm run dev               # tsc --watch
```

## 종료 조건 (필수 게이트)

작업 완료 선언 전에 전부 통과해야 한다.

```bash
cd backend && cargo test        # 단위·회귀 테스트 (Phase 6 기준 544개)
cd frontend && npm run build    # tsc -b + vite build
cd mcp && npm run build         # tsc
```

테스트 문화: 외부 의존(Qdrant·실 LLM) 없는 단위 테스트가 원칙이다 — LLM 판단은
mock provider, 검색·Patrol은 trait(`SearchBackend`/`PatrolExecutor`) mock 주입으로
전 분기를 고정한다. 버그 수정에는 재발 방지 회귀 테스트를 동반한다.

## 코드 컨벤션

- 루트 [`CLAUDE.md`](../CLAUDE.md) — 엔지니어링 원칙(근본 원인 추구, 보수적 리팩토링,
  검증된 사실 기반)과 에이전트 작업 규약. 이 레포의 코드·커밋은 이 문서를 따른다.
- 핵심 설계 불변식([overview.md](overview.md))을 깨는 변경은 리뷰에서 반려된다 —
  특히 "정보 유실 0"과 "raw JSON = SSoT".
- 하위호환: raw JSON 스키마 변경은 `#[serde(default)]` / `Option` 원칙
  (→ [data-model.md](data-model.md)).
- 커밋 메시지는 conventional 접두(`feat:`/`fix:`/`docs:`/`deploy:` 등) + 한국어 요약.

## 레포 구성 요소별 안내

| 경로 | 성격 | 편집 시점 |
|------|------|-----------|
| `docs/` | **사실 레퍼런스 (SSoT)** | 코드 동작·구조가 바뀔 때 같은 PR에서 갱신 |
| `contexts/` | 에이전트 작업 컨텍스트 (정체성·정책·스펙·플랜·결정 로그) | 새 작업 착수·정책 합의·주요 결정 시 |
| `prd-maia-brain/` | Phase 1~6 PRD — 역사 기록 | 갱신하지 않는다 (새 대형 작업은 새 PRD) |
| `backend/static/` | 레거시 정적 UI | 미사용 (컨테이너는 frontend 빌드를 서빙) — 정리 후보 |

문서 갱신 규칙의 상세(코드→문서 매핑, 검증 배지)는 [docs/README.md](README.md) 참조.

## 새 기능을 붙이는 표준 지점

- **새 LLM provider**: `backend/src/llm/`에 구현 + `ProviderType` 분기
  (`valid_for_*`, 팩토리) + 설정 API 연결 (→ [llm-providers.md](llm-providers.md))
- **새 커넥터 타입**: `Connector` trait 구현 + `build_connector` 팩토리 한 줄 +
  `ConnectorSpec` variant (→ [ingest.md](ingest.md))
- **새 Patrol 탐지기**: `patrol/detectors.rs`에 순수 함수 추가 + `combine` 등록
  (→ [patrol.md](patrol.md))
- **새 MCP tool**: 대응 REST API를 먼저 만들고 `mcp/src/index.ts`에 얇은 번역만 추가
  (`contexts/decision_log.md` DEC-004 — REST가 유니버설 인터페이스)
