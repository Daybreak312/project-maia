# Development Plan

## Phase 1: MVP

### Step 1: 프로젝트 셋업
- [x] 기획 문서 작성
- [x] Rust 프로젝트 초기화
- [x] 의존성 설정 (Cargo.toml)
- [x] 기본 디렉토리 구조 생성

### Step 2: 데이터 모델
- [x] Document 구조체 정의
- [x] Entity, Tag 타입 정의
- [x] API Request/Response 타입 정의

### Step 3: 외부 연동
- [x] Qdrant 클라이언트 모듈
- [x] LLM Provider 추상화 (trait)
- [x] Gemini/Claude/OpenAI Provider 구현

### Step 4: 핵심 로직
- [x] LLM Provider 기반 파싱
- [x] 임베딩 생성 (Provider별)
- [x] 인덱싱 오케스트레이션
- [x] 하이브리드 검색 (Vector + BM25 + RRF)

### Step 5: API 서버
- [x] axum 서버 셋업
- [x] POST /ingest, POST /search
- [x] GET/PUT/DELETE /documents/{id}
- [x] GET /recent, GET /tags
- [x] POST /api/reindex
- [x] 설정 API (/api/settings)

### Step 6: 설정 관리
- [x] Settings 구조체 정의
- [x] 파일 기반 저장/로드 (settings.json)
- [x] Provider별 API Key 관리

### Step 7: 프론트엔드
- [x] React + Vite 마이그레이션
- [x] Add / Search / Browse / Admin 페이지
- [x] 로고 추가

### Step 8: API 인증
- [x] MAIA_API_KEY 환경변수 기반 Bearer 토큰 인증
- [x] axum 미들웨어로 API 라우트 보호
- [x] /health 엔드포인트 인증 제외

### Step 9: MCP 서버
- [x] TypeScript 프로젝트 초기화 (mcp/)
- [x] Maia REST API HTTP 클라이언트 (maia-client.ts)
- [x] MCP Tool 5개 등록 (search, ingest, get_document, list_recent, get_tags)
- [x] MAIA_API_KEY 헤더 전달

### Step 10: 모노레포 구조 전환
- [x] 백엔드 코드 → backend/ 이동
- [x] 프론트엔드 → frontend/ (루트 레벨)
- [x] MCP 서버 → mcp/
- [x] .gitignore 경로 업데이트

### Step 11: 통합 테스트
- [x] Qdrant 로컬 실행
- [x] Gemini API Key 등록
- [x] 전체 플로우 검증 (Ingest → Search → MCP Tool 호출)
- [x] 검색 점수 정직성 검증 (raw cosine similarity + 동적 필터링)

### Step 12: Atomic Fact 청킹
- [x] ParsedContent, Document, SearchResult에 facts/matched_facts 필드 추가
- [x] LLM 파싱 프롬프트에 atomic fact 추출 규칙 추가
- [x] 3개 LLM Provider (Gemini/Claude/OpenAI)에 facts 필드 매핑
- [x] Qdrant: ChunkData 구조체, upsert_chunks, delete_by_document_id, payload index
- [x] Indexer: build_chunks, ingest/update/delete/reindex 전부 chunk 기반 변환
- [x] Vector 검색: chunk 단위 검색 → document_id 그룹핑 → matched_facts 수집
- [x] Keyword 검색: summary chunk만 대상으로 BM25
- [x] MCP 타입 업데이트 (matched_facts, facts)

---

## Phase 2: 확장 (Future)

### 웹 AI 연동
- [ ] ChatGPT GPT Actions (OpenAPI spec)
- [ ] Gemini Google Extensions

### 프론트엔드 개선
- [ ] 태그/엔티티 수동 편집
- [ ] 시각화 (타임라인)

### 자동 수집
- [ ] Threads 크롤러
- [ ] GeekNews 크롤러

### 프로젝트 분리
- [ ] 멀티 collection 지원
- [ ] 프로젝트별 설정

---

## Current Status

**현재 단계**: Phase 1 MVP 완료
**완료**: 모노레포 구조, MCP 서버, API 인증, 통합 테스트, Atomic Fact 청킹
**다음 작업**: Phase 2 확장 기능 (웹 AI 연동, 프론트엔드 개선, 자동 수집)
