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
