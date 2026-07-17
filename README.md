# Maia — 개인 인공두뇌

Maia는 소유자의 모든 정보(일상 기록·프로젝트·관심사·의사결정)를 저장하는 원장이자,
AI 에이전트들(Claude Code, OpenClaw 등)이 공유하는 **문맥 엔진**이다. "문서를 잘 찾는
RAG 앱"이 아니라 **에이전트가 사람처럼 일하도록 문맥을 제공하는 메모리 레이어**를 지향한다.

- **Backend** (`backend/`): Rust + axum. 원장(raw JSON, SSoT) + Qdrant(파생 벡터 인덱스).
- **MCP** (`mcp/`): TypeScript. REST API를 MCP tool 6종으로 노출하는 얇은 브릿지.
- **Frontend** (`frontend/`): React + Vite. 관리/검색/브라우즈/거버넌스 UI.

## 📚 문서

**[`docs/`](docs/README.md)가 사실 레퍼런스의 SSoT다.** 아키텍처·데이터 모델·검색·유입·
Patrol·LLM 프로바이더·API·MCP·배포·운영 런북·알려진 이슈가 문서별로 정리돼 있고,
상황별 읽기 경로와 유지보수 규약은 [docs/README.md](docs/README.md)에 있다.

| 급한 용무 | 바로가기 |
|-----------|----------|
| 처음 파악 | [docs/overview.md](docs/overview.md) |
| API 호출 | [docs/api.md](docs/api.md) · [docs/mcp.md](docs/mcp.md) |
| 배포/이전 | [docs/deployment.md](docs/deployment.md) |
| 장애 대응 | [docs/operations.md](docs/operations.md) |

## 빠른 시작

```bash
cp .env.example .env   # MAIA_API_KEY 반드시 설정 (미설정 = 인증 비활성 개발 모드)
docker compose -f docker-compose.local.yml up -d --build
# 앱: http://127.0.0.1:9080  (Qdrant는 내부 네트워크 전용)
```

> ⚠️ 원격 노출 배포에서 `MAIA_API_KEY` 미설정은 곧 전면 공개다 —
> [docs/deployment.md](docs/deployment.md)의 보안 체크리스트를 따를 것.

`cargo test`(backend), `npm run build`(frontend/mcp)가 종료 조건이다
(→ [docs/development.md](docs/development.md)).

## 모델 프로바이더 — 종량제 키 없이 자립

파싱은 **구독**(Claude OAuth / ChatGPT Codex), 임베딩은 **로컬 연산**으로 돌 수 있다.

| 용도 | 선택지 | 활성화 |
|------|--------|--------|
| 파싱 | gemini · **claude**(API키/구독 OAuth, `claude-sonnet-5`) · openai · **codex**(구독, `gpt-5.5`) | 키 등록 또는 auth.json 임포트 |
| 임베딩 | gemini(3072d) · openai(1536d) · **local**(384d, 키 불요) | Admin UI / 설정 API |

교차 제약: local은 임베딩 전용, codex는 파싱 전용, claude는 임베딩 미지원.
전환 절차(런북)와 차원 마이그레이션은 [docs/operations.md](docs/operations.md),
프로바이더 상세는 [docs/llm-providers.md](docs/llm-providers.md) 참조.

## 설계 불변식 (요약)

1. **정보 유실 0** — 파싱·임베딩·refresh가 전부 실패해도 raw 저장은 성공한다.
2. **raw JSON = SSoT** — Qdrant는 파생 인덱스. `POST /api/reindex`만으로 그래프 엣지까지
   전체 복원.
3. **반자율 거버넌스** — 시스템은 플래그만 세우고, 삭제는 사람이 판단한다.

전체 목록과 근거는 [docs/overview.md](docs/overview.md).

## 상태

Phase 1(기반)~6(구독 프로바이더·로컬 임베딩) 완료, **운영 단계**. Phase별 PRD는
[`prd-maia-brain/`](prd-maia-brain/00-overview.md), 개선 대기 항목은
[docs/known-issues.md](docs/known-issues.md), 에이전트 작업 컨텍스트는
[`contexts/`](contexts/primary.md) 참조.
