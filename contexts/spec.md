# Feature Specification

## Phase 1: MVP

### 1.1 정보 입력 (Ingest)

**Input**: 자연어 텍스트
```
"오늘 A회사 면접 봤는데, 연봉 8천 제안받음.
 분위기는 좋았는데 기술스택이 레거시야"
```

**Process**:
1. 원본 저장 (raw JSON 파일)
2. LLM으로 파싱 (선택된 Provider 사용)
   - 개요(summary) 생성
   - 태그 추출
   - 엔티티 추출 (회사명, 금액, 날짜, 스킬 등)
3. 임베딩 생성 (summary 기반)
4. Qdrant에 벡터 인덱싱

**Output**: 저장 완료 + 추출된 메타데이터 반환

### 1.2 검색 (Search)

**Input**: 자연어 쿼리 + 검색 모드 선택

**Search Modes**:
- `hybrid` (기본): 벡터 + 키워드 검색 결과를 RRF로 결합
- `vector`: 쿼리 임베딩 → Qdrant 코사인 유사도 검색
- `keyword`: 전체 문서 BM25 스코어링

**Output**:
```json
{
  "results": [{ "id": "...", "summary": "...", "tags": [...], "relevance_score": 0.85 }],
  "sources_used": ["doc_001", "doc_003"],
  "total": 15,
  "mode": "hybrid"
}
```

### 1.3 AI 모델 추상화

**지원 모델**:
| Provider | 파싱 모델 | 임베딩 모델 | 임베딩 차원 |
|----------|-----------|-------------|-------------|
| Gemini | gemini-2.5-flash | gemini-embedding-001 | 768 |
| Claude | claude-sonnet-4-20250514 | (OpenAI 폴백) | - |
| OpenAI | gpt-4o-mini | text-embedding-3-small | 1536 |

- API Key는 `data/settings.json`에 파일 기반 저장
- 런타임에 Provider 변경 가능
- Provider 변경 시 임베딩 차원이 다르면 `POST /api/reindex` 필요

### 1.4 프론트엔드 UI (React + Vite)

**페이지 구성**:
1. **Add** (`/`) — 자연어 입력으로 정보 추가
2. **Search** (`/search`) — 하이브리드 검색, 모드 선택
3. **Browse** (`/browse`) — 전체 문서 브라우징
4. **Admin** (`/admin`) — API Key 관리, Provider 선택

### 1.5 MCP 서버

로컬 MCP 서버(TypeScript, STDIO transport)가 원격 Maia 백엔드의 REST API를 호출하는 브릿지 구조.

**등록 Tool**:
| Tool | 트리거 예시 | Maia API |
|------|-------------|----------|
| `search_context` | "내 면접 경험 알려줘" | `POST /search` |
| `ingest_information` | "이거 기억해둬" | `POST /ingest` |
| `get_document` | 검색 결과 원문 조회 | `GET /documents/{id}` |
| `list_recent_documents` | "최근에 뭘 저장했지?" | `GET /recent` |
| `get_tags` | 태그 목록 확인 | `GET /tags` |

**호환 AI 도구**: Claude Desktop, Claude Code, Cursor, VS Code Copilot, Gemini CLI

### 1.6 API 인증

- `MAIA_API_KEY` 환경변수로 Bearer 토큰 인증
- 미설정 시 인증 비활성화 (로컬 개발)
- `/health`만 인증 불필요

---

## API Endpoints

```
# 인증 필요 (MAIA_API_KEY 설정 시)
POST   /ingest                          — 정보 저장
POST   /search                          — 검색
GET    /documents/{id}                  — 문서 조회
PUT    /documents/{id}                  — 문서 수정 (재파싱 + 재임베딩)
DELETE /documents/{id}                  — 문서 삭제
GET    /recent?limit=20&offset=0        — 최근 문서 목록
GET    /tags                            — 전체 태그 목록
POST   /api/reindex                     — 전체 문서 재인덱싱
GET    /api/settings                    — 설정 조회
PUT    /api/settings                    — Provider 변경
POST   /api/settings/models/{provider}/key   — API Key 설정
DELETE /api/settings/models/{provider}/key   — API Key 삭제
POST   /api/settings/models/{provider}/test  — API Key 유효성 검증

# 인증 불필요
GET    /health                          — 헬스체크
```

---

## 배포 아키텍처

### Docker Compose 구성
```
services:
  qdrant       — 벡터 DB
  app (maia)   — Rust 백엔드 (:8080)
  nginx        — 리버스 프록시 (HTTPS, :443)
```

### MCP 클라이언트 설정 (claude_desktop_config.json)
```json
{
  "mcpServers": {
    "maia": {
      "command": "node",
      "args": ["/path/to/project-maia/mcp/dist/index.js"],
      "env": {
        "MAIA_URL": "https://your-server:8080",
        "MAIA_API_KEY": "your-secret-key"
      }
    }
  }
}
```

---

## Phase 2: 확장 (Future)

### 2.1 웹 AI 연동
- ChatGPT GPT Actions (OpenAPI spec)
- Gemini Google Extensions

### 2.2 자동 수집
- Threads, GeekNews 등 크롤링

### 2.3 프로젝트 분리
- 멀티 collection 지원
