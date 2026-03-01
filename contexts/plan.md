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

### Step 5: API 서버
- [x] axum 서버 셋업
- [x] POST /ingest 엔드포인트
- [x] POST /search 엔드포인트
- [x] GET /documents/{id} 엔드포인트
- [x] 설정 API (/api/settings)

### Step 6: 설정 관리
- [x] Settings 구조체 정의
- [x] 파일 기반 저장/로드 (settings.json)
- [x] Provider별 API Key 관리

### Step 7: 프론트엔드
- [x] 정적 파일 서빙 설정
- [x] 메인 페이지 (정보 추가)
- [x] 검색 페이지
- [x] 어드민 페이지 (API Key 관리)

### Step 8: 통합 테스트
- [ ] Qdrant 로컬 실행
- [ ] Gemini API Key 등록
- [ ] 전체 플로우 검증

---

## Phase 2: 확장 (Future)

### MCP 서버
- [ ] MCP 프로토콜 구현
- [ ] Claude Code 연동 테스트

### 프론트엔드 개선
- [ ] 웹 UI 고도화
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

**현재 단계**: Step 8 - 통합 테스트
**다음 작업**: Qdrant 실행 후 Gemini API Key 등록 및 테스트
