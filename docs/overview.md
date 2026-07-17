# Maia 개요 — 정체성과 설계 원칙

> 최종 검증: 2026-07-17 · 기준 커밋: 712484e · [문서 인덱스](README.md)

## Maia는 무엇인가

Maia는 소유자의 모든 정보(일상 기록·프로젝트·관심사·의사결정)를 저장하는 **개인 지식
원장(ledger)**이자, AI 에이전트들(Claude Code, OpenClaw 등)이 공유하는 **문맥 엔진**이다.
"문서를 잘 찾는 RAG 앱"이 아니라, **에이전트가 사람처럼 일하도록 문맥을 제공하는 메모리
레이어**를 지향한다.

세 가지 실체로 구성된다.

| 컴포넌트 | 기술 | 역할 |
|----------|------|------|
| `backend/` | Rust + axum | 원장(raw JSON, SSoT) + Qdrant(파생 벡터 인덱스) + REST API |
| `mcp/` | TypeScript | REST API를 MCP tool 6종으로 노출하는 얇은 브릿지 |
| `frontend/` | React + Vite | 관리/검색/브라우즈/거버넌스 UI (백엔드가 정적 서빙) |

## 핵심 불변식 (Invariants)

코드 전반이 지키도록 설계·테스트된 규칙들. 수정 작업 시 이 불변식을 깨는 변경은
버그로 간주한다.

1. **정보 유실 0** — 파싱·임베딩·토큰 refresh가 전부 실패해도 raw 저장은 성공한다.
   유입 경로는 LLM 호출 **이전에** raw 문서를 디스크에 기록한다
   (`backend/src/core/indexer.rs`의 `persist_raw_document`). 실패는 `fallback=true`로
   표시될 뿐 에러가 되지 않는다.
2. **raw JSON = SSoT** — 문서의 진실은 `data/workspaces/{id}/documents/*.json`이다.
   Qdrant는 파생 인덱스일 뿐이며, `POST /api/reindex`만으로 그래프 엣지를 포함한
   전체 검색 상태를 복원할 수 있다. → [data-model.md](data-model.md)
3. **fail-safe 폴백** — 검색 에이전트(LLM) 실패 시 초기 hybrid 결과를 그대로 반환하고,
   초기 검색 실패 시 빈 결과 + 폴백 표시(500 금지). 단, 임베딩 차원 불일치만큼은
   침묵하지 않고 명시적 에러를 낸다(원인 은폐 방지). → [search.md](search.md)
4. **LLM 호출 상수 상한** — 유입 판단은 문서당 최대 2회, deep search는 라운드당
   상한(재작성 3회 / LLM 5회 / 시간 제한)이 있어 비용이 입력 크기에 비례 폭주하지 않는다.
5. **반자율 거버넌스** — Patrol은 읽기 + 플래그 + 엣지 감쇠 재계산만 한다. 문서의 변경·삭제는
   오직 사람의 판단(Review Queue judge)에서만 일어난다. → [patrol.md](patrol.md)
6. **종량제 키 탈피 (Phase 6)** — 파싱은 구독(Claude OAuth / ChatGPT Codex), 임베딩은
   로컬 연산(multilingual-e5-small)으로 돌 수 있어 종량제 API 키 없이 자립 가능하다.
   → [llm-providers.md](llm-providers.md)
7. **워크스페이스 격리** — 문서·컬렉션·설정·키 스코프의 단위. 영속 API 키는 명시된
   워크스페이스에만 접근한다(fail-closed). → [api.md](api.md)

## 용어집

| 용어 | 의미 |
|------|------|
| 문서(Document) | 저장 단위. 원문(raw_content) + 파싱 결과(summary/entities/facts) + 엣지 + 출처 |
| 청크(Chunk) | Qdrant 인덱싱 단위. 문서당 summary 1개 + fact N개 |
| 엣지(Edge) | 문서 간 방향성 관계. related_to / updates / contradicts / references / part_of |
| 워크스페이스 | 격리된 지식 공간. 기본값 `default` |
| 커넥터 | 외부 소스(로컬 디렉토리 등)에서 정보를 주기적으로 끌어오는 유입 계층 |
| Patrol | 기억 상태를 주기 점검해 이상 후보를 Review Queue에 올리는 자기 관리 계층 |
| Review Queue | Patrol이 세운 플래그를 사람이 판단(유효/수정/삭제/기각)하는 큐 |
| SSoT | Single Source of Truth. Maia에서는 raw JSON 문서 파일 |
| smart ingest | LLM이 신규/업데이트/분할/중복을 판단해 저장하는 기본 유입 모드 |
| deep search | 충분성 평가·쿼리 재작성·그래프 확장을 반복하는 능동 회상 검색 |

## 발전 이력

설계 결정의 역사적 맥락은 [`prd-maia-brain/`](../prd-maia-brain/00-overview.md)(Phase별 PRD)과
[`contexts/decision_log.md`](../contexts/decision_log.md)에 있다.

| Phase | 내용 | 상태 |
|-------|------|------|
| 1 | 기반 완성 — 워크스페이스 시스템, API 키 인증, BM25 견고화 | 완료 |
| 2 | Brain Core — 지식 그래프(엣지), Ingest Agent, 버전 보관 | 완료 |
| 3 | Search Agent — deep search(충분성 평가·재작성·그래프 확장) | 완료 |
| 4 | Connectors — 로컬 디렉토리 커넥터, 스케줄러, 대량 적재 | 완료 |
| 5 | Patrol — 탐지기 4종, Review Queue, 엣지 감쇠, 메트릭 | 완료 |
| 6 | 구독 프로바이더(Claude OAuth·Codex) & 로컬 임베딩 | 완료 |

현재는 **운영 단계**다. 실 배포는 Oracle Cloud ARM 인스턴스에서 docker compose로 돌며
([deployment.md](deployment.md)), OpenClaw 워크스페이스의 메모리·리포트가 커넥터로
지속 유입된다.
