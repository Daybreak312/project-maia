# MCP 서버

> 최종 검증: 2026-07-25 · 기준 커밋: 9cbd366 · [문서 인덱스](README.md)

## 구조

`mcp/`는 Maia REST API를 MCP tool로 노출하는 **얇은 브릿지**다(비즈니스 로직 없음).
TypeScript + `@modelcontextprotocol/sdk`, STDIO transport — AI 도구가 프로세스를 직접
spawn하므로 별도 포트가 없다. 서버 이름 `maia`.

```
AI 도구 (Claude Code / Desktop / OpenClaw …)
  └─ spawn → node mcp/dist/index.js ── HTTP(Bearer) ──▶ Maia 서버(들)
                └─ 서버 레지스트리 (alias → url/key/workspace)
```

Maia는 온프레미스 설치가 기본 전제다. 한 사용자가 개인 서버와 조직 서버를 병용하는
상황(personal / enterprise …)을 위해 브릿지 하나가 **다중 서버 레지스트리**를 들고,
모든 tool이 선택적 `server` 인자로 대상 서버를 고른다 (`mcp/src/servers.ts`).

## 설치

각 Maia 서버가 설치 수단을 직접 배포한다(셀프 배포 — 폐쇄망에서 외부 저장소 불필요):

| 엔드포인트 | 내용 |
|-----------|------|
| `GET /mcp` | 사람용 설치 안내 (Markdown, 요청 도메인 자동 반영) |
| `GET /mcp/install.json` | 기계용 매니페스트 (JSON) |
| `GET /mcp/client.tar.gz` | 클라이언트 소스 번들 (이미지 빌드 시 패키징) |

세 엔드포인트는 비밀이 없어 인증 밖(public)이다 (`backend/src/api/mcp.rs`).
관련 서버 환경변수: `MAIA_PUBLIC_URL`(베이스 URL 강제 지정 — 미설정 시
`X-Forwarded-Proto`+`Host` 헤더로 유도), `MAIA_REPO_URL`(내부 미러 안내),
`MCP_CLIENT_TARBALL`(번들 경로, 기본 `/app/mcp-client.tar.gz`).

소스에서 직접 빌드하는 경우:

```bash
cd mcp && npm ci && npm run build   # tsc → dist/index.js
```

## 서버 레지스트리 (`mcp/src/servers.ts`)

설정 소스 우선순위 — **첫 번째로 발견된 소스 하나만 사용한다(병합 없음)**:

1. `MAIA_SERVERS` — 인라인 JSON (레지스트리 문서 전체)
2. `MAIA_SERVERS_FILE` — 레지스트리 JSON 파일 경로 (없으면 에러)
3. `~/.maia/servers.json` — 기본 경로 (존재할 때)
4. 레거시 env — `MAIA_URL`(기본 `http://localhost:8080`) / `MAIA_API_KEY` /
   `MAIA_WORKSPACE` → 단일 서버 alias `default`

발견된 소스가 깨져 있으면 다음 소스로 폴백하지 않고 **기동 실패(fail-fast)** —
잘못된 서버로의 조용한 접속을 막는다. 기동 시 stderr에 로드 소스·서버 목록을 남긴다.

레지스트리 문서 형식:

```json
{
  "defaultServer": "personal",
  "servers": {
    "personal": {
      "url": "https://maia.daybreak.cloud",
      "apiKeyFile": "~/.maia/personal.key",
      "workspace": "default"
    },
    "enterprise": { "url": "https://maia.corp.example.com", "apiKey": "..." }
  }
}
```

- `defaultServer`: `server` 인자 생략 시 대상. 서버 2개 이상이면 필수, 1개면 그 서버.
- `apiKey`/`apiKeyFile`은 상호 배타(둘 다 지정 시 에러). `apiKeyFile`은 `~/` 전개 지원.
- `workspace`: 그 서버로 향하는 호출의 기본 워크스페이스.

## Tool 레퍼런스 (6종)

모든 tool은 선택적 `workspace`·`server` 인자를 받는다.

- workspace 우선순위: tool 인자 > 대상 서버의 `workspace` > 서버가 키에서 유도한 기본값
- server: 레지스트리 alias. 생략 시 `defaultServer`. 미등록 alias는 에러(설정된 목록 안내).
  인자 설명(description)에 설정된 alias·URL 목록이 노출되어 호출자(LLM)가 고를 수 있다.

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

### 레지스트리 사용 (권장)

`~/.maia/servers.json`을 만들어 두면 AI 도구 설정에는 env 없이 등록한다:

```json
{
  "mcpServers": {
    "maia": {
      "command": "node",
      "args": ["/path/to/maia-mcp/dist/index.js"]
    }
  }
}
```

### 단일 서버 (레거시 env)

```json
{
  "mcpServers": {
    "maia": {
      "command": "node",
      "args": ["/path/to/maia-mcp/dist/index.js"],
      "env": {
        "MAIA_URL": "https://maia.daybreak.cloud",
        "MAIA_API_KEY": "<발급 키>",
        "MAIA_WORKSPACE": "default"
      }
    }
  }
}
```

### OpenClaw

게이트웨이 설정에 stdio MCP 서버로 등록한다 (등록명 `maia`, command/env는 위와 동일).

## 운영 팁

- MCP 프로세스는 상태가 없다 — 레지스트리(주소·키)만 맞으면 어디서든 동일하게 동작한다.
- 전용 키 발급을 권장한다: 웹 UI 계정 페이지의 셀프서비스 키 또는 admin `POST /api/keys`
  (워크스페이스 스코프 + `read_write`) — 마스터키 배포 금지 (→ [api.md](api.md)).
- 검색 결과가 비면 백엔드 `/health` → 인증(401 여부) → 차원 불일치 에러 순으로 확인
  (→ [operations.md](operations.md) 트러블슈팅).
- 설정이 어느 소스에서 로드됐는지는 브릿지 기동 직후 stderr 로그
  (`[maia-mcp] loaded N server(s) from …`)로 확인한다.
