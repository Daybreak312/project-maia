# Feature Specification

## Phase 1: MVP (현재)

### 1.1 정보 입력 (Ingest)

**Input**: 자연어 텍스트
```
"오늘 A회사 면접 봤는데, 연봉 8천 제안받음.
 분위기는 좋았는데 기술스택이 레거시야"
```

**Process**:
1. 원본 저장 (raw)
2. LLM으로 파싱 (선택된 모델 사용)
   - 개요(summary) 생성
   - 태그 추출
   - 엔티티 추출 (회사명, 금액, 날짜 등)
3. 임베딩 생성 (summary 기반)
4. Qdrant에 인덱싱

**Output**: 저장 완료 + 추출된 메타데이터 반환

### 1.2 검색 (Search)

**Input**: 자연어 쿼리
```
"작년에 면접 봤던 회사들 연봉 비교해줘"
```

**Process**:
1. 쿼리 임베딩 생성
2. Qdrant에서 하이브리드 검색
3. 결과 반환 + 사용된 소스 명시

**Output**:
```json
{
  "results": [...],
  "sources_used": ["doc_001", "doc_003"]
}
```

### 1.3 AI 모델 추상화

**지원 모델**:
- Gemini (구현 완료)
- Claude (스텁)
- GPT (스텁)

**용도별 모델 선택**:
- `parsing`: 정보 입력 시 개요 요약/태그/엔티티 추출
- `embedding`: 벡터 임베딩 생성

**설정 저장**:
- API Key는 파일 기반 저장 (`data/settings.json`)
- 런타임에 모델 변경 가능

### 1.4 프론트엔드 UI

**페이지 구성**:
1. **정보 추가** (`/`) - 메인 페이지, 자연어 입력
2. **검색** (`/search`) - 검색 UI
3. **어드민** (`/admin`) - 설정 관리
   - AI 모델별 API Key 등록
   - 용도별 모델 선택
   - (향후) 프로젝트 관리

**기술 스택**: 정적 HTML + Vanilla JS (MVP)

---

## API Endpoints

```
# 기존 API
POST /ingest
POST /search
GET  /documents/{id}
GET  /recent

# 설정 API
GET  /api/settings              - 현재 설정 조회
PUT  /api/settings              - 설정 변경
GET  /api/settings/models       - 등록된 모델 목록
POST /api/settings/models/{provider}/test - API Key 유효성 검증

# 정적 파일
GET  /                          - 프론트엔드 UI
GET  /static/*                  - 정적 자원
```

---

## 배포 아키텍처

### Docker Compose 구성
```
nginx (443/80)
  ├── / → Frontend (dist)
  └── /api → Backend (Rust)

services:
  - qdrant
  - backend (maia)
  - nginx + certbot (HTTPS)
```

### 프론트엔드
- 현재: 정적 HTML
- 배포 시: Vite 빌드 → dist 폴더
- 백엔드에서 dist 서빙

### 라우팅
- `/api/*` → Backend API
- `/admin` → Admin UI
- `/` → Main UI (정보 추가/검색)

### HTTPS
- nginx reverse proxy
- certbot (Let's Encrypt)

---

## Phase 2: 확장 (Future)

### 2.1 MCP 서버
- Claude Code 등 외부 도구에서 접근

### 2.2 자동 수집
- Threads, GeekNews 등 크롤링

### 2.3 프로젝트 분리
- 멀티 collection 지원
