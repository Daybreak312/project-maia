# Decision Log

## DEC-001: MCP 서버를 TypeScript Thin Wrapper로 구현

**일시**: 2026-03-02
**결정**: Rust 네이티브 MCP 내장 대신, 별도 TypeScript MCP 서버가 Maia REST API를 HTTP로 호출하는 구조 채택

**근거**:
- MCP TypeScript SDK가 가장 성숙하고 레퍼런스 풍부
- 기존 Rust 백엔드 코드 변경 제로
- MCP 서버는 얇은 번역 레이어일 뿐, 비즈니스 로직 없음
- STDIO transport로 별도 포트 불필요 (AI 도구가 프로세스 직접 spawn)

**대안 검토**:
- Rust MCP SDK (rmcp): 단일 바이너리 유지 가능하나 SDK 성숙도 부족
- Spring Boot MCP Starter: JVM 추가, 기술 스택 이질감

---

## DEC-002: 모노레포 구조 전환 (backend/ + mcp/ + frontend/)

**일시**: 2026-03-02
**결정**: 루트에 있던 백엔드 코드를 `backend/`로, 프론트엔드를 `frontend/`로 분리. `mcp/` 추가.

**근거**:
- MCP, Frontend, Backend가 독립적인 빌드 단위
- 각 모듈의 관심사 명확히 분리
- `git mv`로 히스토리 보존

---

## DEC-003: API 키 인증 (Bearer Token)

**일시**: 2026-03-02
**결정**: `MAIA_API_KEY` 환경변수 기반 Bearer 토큰 인증을 axum 미들웨어로 구현

**근거**:
- EC2/온프레미스에 배포하므로 외부 노출 시 인증 필수
- 개인용 시스템이므로 단일 정적 API 키로 충분
- 미설정 시 인증 비활성화로 로컬 개발 편의성 유지
- `/health`만 인증 제외 (모니터링/헬스체크)

---

## DEC-004: Maia REST API를 유니버설 인터페이스로

**일시**: 2026-03-02
**결정**: MCP, GPT Actions, 기타 미래의 어댑터 모두 Maia REST API를 호출하는 구조

**근거**:
- Maia 백엔드가 Single Source of Truth
- 어댑터(MCP, GPT Action 등)는 프로토콜 번역만 담당
- 새로운 AI 도구 연동 시 얇은 어댑터만 추가하면 됨
- 백엔드 변경 없이 연동 확장 가능

---

## DEC-005: Atomic Fact 청킹으로 검색 정밀도 향상

**일시**: 2026-03-02
**결정**: 1 Document = 1 Vector(summary) 구조를 1 Document = N Vectors(summary + facts) 구조로 전환

**근거**:
- 긴 문서의 summary가 모든 세부 정보를 대표하지 못해 검색 누락 발생
- LLM이 문서를 독립적 사실 문장으로 분해 → 각 fact에 개별 임베딩 부여
- 어떤 각도의 질문이든 해당 사실의 벡터에 직접 매칭 가능
- `#[serde(default)]`로 기존 문서 하위호환, reindex로 점진적 마이그레이션

**구현 핵심**:
- Qdrant Point ID: chunk별 랜덤 UUID, `document_id` payload 필드로 문서 소속 식별
- 삭제: `document_id` 필터 기반 (1개 문서 = N개 point 일괄 삭제)
- 검색: chunk 단위 유사도 → document 그룹핑 → matched_facts 수집

---

## DEC-006: 검색 점수를 Raw Cosine Similarity로 표시

**일시**: 2026-03-02
**결정**: RRF 정규화 점수 대신 벡터 검색의 raw cosine similarity를 사용자에게 표시

**근거**:
- RRF 정규화 점수(0.96, 0.93 등)는 상대적 순위이지 절대적 관련도가 아님
- 모든 결과가 90%+ 표시되어 사용자에게 오해 유발
- Raw cosine similarity는 정직한 절대적 유사도 지표
- 동적 필터링(절대 임계 0.5 + 점수 드롭 0.15 + 상한 5개)으로 노이즈 제거

---

## DEC-007: raw JSON = SSoT, 신규 DB 금지, 무조건 raw 폴백

**일시**: 2026-07-06 (prd-maia-brain 킥오프)
**결정**: 그래프 엣지를 포함한 모든 진실을 raw JSON 파일에 두고, Qdrant는 reindex로
전량 재생성 가능한 파생 인덱스로 유지. 신규 DB 도입 금지. LLM·임베딩이 실패해도
raw 저장은 반드시 성공(정보 유실 0).

**근거**: 개인 기억 시스템에서 최우선 가치는 "잃지 않는 것"과 "들고 떠날 수 있는 것".
DB 추가는 운영 부담·이식성 저하 대비 이득 없음.

---

## DEC-008: Phase 6 — 구독 프로바이더 + 로컬 임베딩으로 종량제 탈피

**일시**: 2026-07-07
**결정**: 파싱은 Claude 구독 OAuth(`sk-ant-oat` 자동 감지)와 ChatGPT Codex(auth.json
임포트), 임베딩은 로컬 fastembed(multilingual-e5-small, 384d)를 추가. 기존
Gemini/OpenAI provider는 유지한 채 능력만 확장. LLM HTTP 타임아웃 60→300초
(thinking 모델의 대형 문서 파싱 실측).

**근거**: 종량제 키 의존 제거 — 소유자가 이미 지불 중인 구독과 로컬 연산으로 자립.

---

## DEC-009: Oracle Cloud ARM으로 호스팅 이전

**일시**: 2026-07-08
**결정**: 맥 로컬 호스팅에서 Oracle ARM 인스턴스(동거 서비스 존재)로 스택 전체 이전.
CPU 캡(cpu_shares 512) + `127.0.0.1` 바인딩 + SSH 터널 접근. 이미지는 레지스트리 없이
save/load 이송 (`deploy/oracle/docker-compose.yml`).

**근거**: 로컬 임베딩·파싱의 맥 리소스 부담 제거, 상시 가용성 확보. 클라이언트 설정은
터널 덕에 무변경.

---

## DEC-010: 파싱 모델 sonnet-5 전환 + 응답 견고화

**일시**: 2026-07-17 (커밋 712484e)
**결정**: Claude 파싱 모델 `claude-haiku-4-5` → `claude-sonnet-5`. 동시에
`max_tokens` 16384/8192 상향, `stop_reason=max_tokens` 절단 가드, ContentBlock
tagged enum(thinking 블록 관용 수용).

**근거**: 운영 사고 2건 실측 — ① 출력 상한 1024에서 대형 문서 JSON이 절단되어
"파싱 실패"로 원인 은폐 ② sonnet-5의 adaptive thinking 블록이 경직된 역직렬화를
붕괴. "침묵 실패 금지" 원칙의 직접 적용.

---

## DEC-011: 레포 공개 전환

**일시**: 2026-07-17
**결정**: https://github.com/Daybreak312/project-maia 공개(라이선스 미설정, 전체
히스토리 포함). 공개 전 전 히스토리 시크릿 스캔 0건 확인.

**근거**: 소유자 결정. 이후 커밋은 공개를 전제로 한다 — 크리덴셜·개인 배포 비밀
커밋 금지 (policy.md `[Security] - [Repo]`).

---

## DEC-012: 문서 체계 개편 — docs/ 신설

**일시**: 2026-07-17
**결정**: 사실 레퍼런스를 `docs/`(인덱스: docs/README.md)로 분리·신설하고
`contexts/architecture.md`는 스텁으로 이관. `contexts/`는 작업 컨텍스트(정체성·정책·
현재 스펙·현황판·결정 로그) 전용으로 재정의, `prd-maia-brain/`은 역사 기록으로 동결.
문서마다 "최종 검증일 + 기준 커밋" 배지, 코드→문서 매핑표로 동시 갱신 규약 수립.

**근거**: 단일 architecture.md(537줄)가 레퍼런스·작업 컨텍스트·역사를 겸하며 비대해져
스테일 항목(모델명·모듈 트리)이 발생. 참조 단위 분리 + 갱신 규약이 부패를 막는
구조적 해법.
