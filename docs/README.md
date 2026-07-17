# Maia 문서 인덱스

Maia의 **사실 레퍼런스 문서(SSoT)** 모음이다. "지금 시스템이 어떻게 동작하는가"에
대한 답은 여기서 찾고, 여기에 없으면 코드가 답이다 — 그리고 그 답을 찾았다면 이
폴더에 반영한다.

## 문서 지도

| 문서 | 다루는 것 | 이럴 때 읽는다 |
|------|-----------|----------------|
| [overview.md](overview.md) | 정체성, 핵심 불변식 7개, 용어집, Phase 이력 | 처음 왔을 때. 설계 판단의 기준이 필요할 때 |
| [architecture.md](architecture.md) | 토폴로지, 모노레포 구조, 스택, 기동 순서, 모듈 지도 | 코드 어디를 봐야 할지 잡을 때 |
| [data-model.md](data-model.md) | Document/Edge, 디스크 레이아웃, Qdrant 스키마, 청킹, 쓰기 직렬화, reindex | 저장 구조를 만지거나 데이터를 직접 다룰 때 |
| [ingest.md](ingest.md) | 유입 파이프라인, Ingest Agent 전략, 커넥터·커서 시맨틱 | 유입이 이상하거나 커넥터를 붙일 때 |
| [search.md](search.md) | 검색 모드·RRF, 한글 토크나이저, deep search, 그래프 확장, 검색 로그 | 검색 품질·동작을 이해/개선할 때 |
| [patrol.md](patrol.md) | 탐지기 4종, Review Queue, 엣지 감쇠, 메트릭 | 거버넌스·기억 관리 기능을 다룰 때 |
| [llm-providers.md](llm-providers.md) | provider 5종 상세, OAuth/토큰, 차원 표, 타임아웃 | 모델·키·프로바이더 문제 전반 |
| [api.md](api.md) | 인증·권한 모델, REST 전체 엔드포인트 레퍼런스 | API를 호출하거나 새 클라이언트를 붙일 때 |
| [mcp.md](mcp.md) | MCP tool 6종, 환경변수, 클라이언트 등록 | AI 도구에 Maia를 연결할 때 |
| [deployment.md](deployment.md) | Dockerfile, compose 3종 비교, 보안 체크리스트 | 배포·이전·노출 구성을 바꿀 때 |
| [operations.md](operations.md) | 런북: provider 전환, reindex, 커넥터, 백업/복구, 트러블슈팅 | **운영 중 문제가 생겼을 때 (첫 진입점)** |
| [known-issues.md](known-issues.md) | 감사에서 확인된 개선 대기 항목 (H/M/L) | 수정 작업을 고르거나 이상 동작의 원인을 짚을 때 |
| [development.md](development.md) | 빌드·테스트 게이트, 컨벤션, 확장 지점 | 코드를 수정하기 전에 |

## 상황별 읽기 경로

- **처음 온 에이전트/개발자**: overview → architecture → (작업 영역의 문서 1개)
- **장애 대응**: operations 트러블슈팅 표 → known-issues → 해당 영역 문서
- **API 연동/클라이언트 제작**: api → mcp
- **배포·이전**: deployment → operations(백업/복구)
- **코드 수정**: development → 해당 영역 문서 → (완료 후 아래 유지보수 규약)

## 문서 계층 — 무엇이 어디에 사는가

| 위치 | 성격 | 신선도 기대 |
|------|------|-------------|
| `docs/` (여기) | 현재 시스템의 **사실** | 항상 최신 — 코드와 함께 갱신 |
| [`../README.md`](../README.md) | 첫인상 + 빠른 시작 | docs/를 가리키는 요약 |
| [`../contexts/`](../contexts/primary.md) | 에이전트 **작업 컨텍스트** (정체성·정책·현재 스펙·플랜·결정 로그) | 작업 단위로 갱신 |
| [`../prd-maia-brain/`](../prd-maia-brain/00-overview.md) | Phase 1~6 PRD | 역사 기록 — 갱신 안 함 |

## 유지보수 규약

이 폴더가 썩지 않게 하는 규칙. **코드를 바꾸면 문서도 같은 커밋(또는 같은 PR)에서
바꾼다.**

### 1. 코드 → 문서 매핑

| 이 코드를 바꾸면 | 이 문서를 갱신한다 |
|------------------|--------------------|
| `backend/src/api/`, `auth/`, 라우터(main.rs) | api.md |
| `backend/src/llm/` (모델 상수 포함) | llm-providers.md (+ 전환 절차 바뀌면 operations.md) |
| `backend/src/core/search*.rs` | search.md |
| `backend/src/core/indexer.rs`, `ingest_agent.rs`, `connectors/` | ingest.md (reindex는 data-model.md) |
| `backend/src/storage/`, `models/`, `workspace/` | data-model.md |
| `backend/src/patrol/` | patrol.md |
| `Dockerfile`, `docker-compose*.yml`, `deploy/` | deployment.md |
| `mcp/src/` | mcp.md |
| 설계 불변식 자체의 변경 | overview.md + 관련 문서 전부 |
| known-issues 항목 해소 | known-issues.md에서 제거 + 관련 문서 서술 갱신 |

### 2. 검증 배지

각 문서 상단의 `> 최종 검증: YYYY-MM-DD · 기준 커밋: <hash>`는 "이 날짜에 코드와
대조했다"는 뜻이다. 문서를 갱신하거나 코드와 대조 확인했을 때만 갱신한다.
**배지가 30일 이상 오래됐거나 해당 영역에 큰 커밋이 있었다면, 읽는 쪽이 먼저
의심하고 코드와 대조 후 배지를 갱신한다.**

### 3. 작성 원칙

- 코드에서 확인한 사실만 적는다. 추측·희망사항은 known-issues의 "권고"에만.
- 수치·상수(모델명, 상한, 포트)는 반드시 원문 대조 후 기입 — 이 문서들의 존재 이유다.
- 파일 경로를 함께 적어 독자가 코드로 점프할 수 있게 한다.
- 문서 간 중복 서술은 최소화하고 링크로 잇는다 (중복은 곧 불일치가 된다).
- 개인 배포의 비밀(실 도메인 계정 정보, 크리덴셜 경로)은 적지 않는다 —
  이 레포는 공개다.
