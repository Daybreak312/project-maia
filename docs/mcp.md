# MCP 서버

> 최종 검증: 2026-07-17 · 기준 커밋: 712484e · [문서 인덱스](README.md)

## 구조

`mcp/`는 Maia REST API를 MCP tool로 노출하는 **얇은 브릿지**다(비즈니스 로직 없음).
TypeScript + `@modelcontextprotocol/sdk`, STDIO transport — AI 도구가 프로세스를 직접
spawn하므로 별도 포트가 없다. 서버 이름 `maia`.

```
AI 도구 (Claude Code / Desktop / OpenClaw …)
  └─ spawn → node mcp/dist/index.js ── HTTP(Bearer) ──▶ Maia REST API
```

## 빌드와 실행

```bash
cd mcp && npm ci && npm run build   # tsc → dist/index.js
```

## 환경변수 (`mcp/src/index.ts`)

| 변수 | 기본값 | 의미 |
|------|--------|------|
| `MAIA_URL` | `http://localhost:8080` | 백엔드 주소 |
| `MAIA_API_KEY` | (없음) | Bearer 토큰. 비우면 인증 헤더 미전송 |
| `MAIA_WORKSPACE` | (없음) | 기본 워크스페이스. 미설정 시 서버가 키의 기본값 사용 |

## Tool 레퍼런스 (6종)

모든 tool은 선택적 `workspace` 인자를 받는다. 우선순위:
tool 인자 > `MAIA_WORKSPACE` > 서버가 키에서 유도한 기본값.

| Tool | 파라미터 | REST 매핑 | 용도 |
|------|----------|-----------|------|
| `search_context` | `query`(필수) · `limit`(1–20, 기본 5) · `mode`(hybrid/vector/keyword, 기본 hybrid) | `POST /search` | 1회성 검색. 개인 기록 관련 질문이면 우선 호출 |
| `deep_search` | `query`(필수) | `POST /search` + `agent: true` | 능동 회상 — 충분성 평가·재작성·그래프 확장. 첫 검색이 불완전하거나 주제 전체 클러스터가 필요할 때 |
| `ingest_information` | `content`(필수) | `POST /ingest` | "기억해둬" 류 저장. 응답에 전략(new/update/split/duplicate/raw)과 폴백 여부 표시 |
| `get_neighbors` | `id`(UUID 필수) · `depth`(1–5, 기본 1) | `GET /documents/{id}/neighbors` | 그래프 이웃 탐색 (관계 타입·깊이·경유 포함) |
| `get_document` | `id`(UUID 필수) | `GET /documents/{id}` | 원문 전체 조회 (요약으로 부족할 때) |
| `list_recent_documents` | `limit`(1–50, 기본 10) | `GET /recent` | 최근 저장 목록 |

`deep_search` 응답에는 탐색 과정 요약(라운드 수·시도 쿼리·그래프 확장·폴백 여부)과
확장 결과의 유래(`expanded_from`)가 포함된다 — 동작 상세는 [search.md](search.md).

## 클라이언트 등록 예시

### Claude Desktop / Claude Code (`claude_desktop_config.json` 등)

```json
{
  "mcpServers": {
    "maia": {
      "command": "node",
      "args": ["/path/to/project-maia/mcp/dist/index.js"],
      "env": {
        "MAIA_URL": "http://127.0.0.1:9080",
        "MAIA_API_KEY": "<발급 키 또는 마스터키>",
        "MAIA_WORKSPACE": "default"
      }
    }
  }
}
```

### OpenClaw

게이트웨이 설정에 stdio MCP 서버로 등록한다 (등록명 `maia`, command/env는 위와 동일).
백엔드가 원격이면 `MAIA_URL`은 로컬 터널 포트를 가리키게 한다
(→ [deployment.md](deployment.md)의 원격 접근 절).

## 운영 팁

- MCP 프로세스는 상태가 없다 — 백엔드 주소·키만 맞으면 어디서든 동일하게 동작한다.
- 전용 키 발급을 권장한다: `POST /api/keys`로 워크스페이스 스코프 + `read_write` 키를
  만들어 마스터키 대신 배포 (→ [api.md](api.md)).
- 검색 결과가 비면 백엔드 `/health` → 인증(401 여부) → 차원 불일치 에러 순으로 확인
  (→ [operations.md](operations.md) 트러블슈팅).
