//! MCP 클라이언트 셀프 배포(self-serve) 엔드포인트.
//!
//! Maia는 온프레미스 설치를 기본 전제로 한다 — 설치되는 조직의 네트워크에서
//! GitHub 등 외부 저장소 접근을 보장할 수 없으므로, 서버가 스스로 MCP 브릿지의
//! 설치 수단을 배포한다:
//!
//! - `GET /mcp`               사람용 설치 안내 (Markdown)
//! - `GET /mcp/install.json`  기계용 매니페스트 (JSON)
//! - `GET /mcp/client.tar.gz` 클라이언트 소스 번들 (이미지 빌드 시 패키징 — Dockerfile 참조)
//!
//! 세 응답 모두 비밀(키·사용자 데이터)이 없어 인증 밖(public 라우트)에 둔다 —
//! 설치 안내는 자격증명 획득 이전 단계이므로 인증 뒤에 두면 목적이 성립하지 않는다.

use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// 공식 소스 저장소. 온프레미스 인스턴스는 `MAIA_REPO_URL`로 내부 미러를 가리킬 수 있다.
const DEFAULT_REPO_URL: &str = "https://github.com/Daybreak312/project-maia";

/// 이미지 빌드 시 패키징되는 클라이언트 번들 경로 기본값 (`MCP_CLIENT_TARBALL`로 재지정).
const DEFAULT_TARBALL_PATH: &str = "/app/mcp-client.tar.gz";

/// 설치 안내 Markdown 템플릿. 요청 시점의 베이스 URL 등으로 치환된다 —
/// 안내문이 자기 자신이 서빙되는 주소를 정확히 가리키게 하기 위함 (온프레미스마다 다름).
const GUIDE_TEMPLATE: &str = r##"# Maia MCP 브릿지 설치 안내

> 이 문서는 Maia 서버(%BASE_URL%)가 직접 제공한다. 서버 버전: %VERSION%
> 기계용 매니페스트: %BASE_URL%/mcp/install.json

Maia MCP 브릿지는 AI 도구(Claude Code / Claude Desktop / OpenClaw / Cursor 등)를
Maia 지식 베이스에 연결하는 STDIO 기반 MCP 서버다. AI 도구가 이 프로세스를 직접
spawn하므로 별도 포트나 상주 데몬이 없고, 백엔드 주소와 API 키만 맞으면 어디서든
동일하게 동작한다.

노출 tool 6종: `search_context` · `deep_search` · `ingest_information` ·
`get_neighbors` · `get_document` · `list_recent_documents`

## 1. 요구사항

- Node.js 20 이상 (`node --version`)

## 2. 소스 받기

두 방법 중 하나를 쓴다. 폐쇄망(온프레미스)이면 A가 외부 저장소 접근 없이 동작한다.

### A. 이 서버에서 직접 다운로드

```bash
curl -fLO %BASE_URL%/mcp/client.tar.gz
tar -xzf client.tar.gz && cd maia-mcp
```

### B. git 저장소

```bash
git clone %REPO_URL%.git
cd project-maia/mcp
```

## 3. 빌드

```bash
npm install
npm run build   # → dist/index.js
```

## 4. API 키 준비

웹 UI(%BASE_URL%)에 로그인해 계정 페이지에서 셀프서비스 API 키를 발급하거나,
관리자에게 워크스페이스 스코프 키 발급을 요청한다 (admin: `POST /api/keys`).
마스터키를 클라이언트에 배포하는 것은 권장하지 않는다.

## 5. AI 도구에 등록

### 다중 서버 레지스트리 (권장 — `~/.maia/servers.json`)

Maia는 온프레미스 설치가 기본 전제라 개인 서버와 조직 서버를 함께 쓰는 상황이 흔하다.
`~/.maia/servers.json`에 alias → 접속 정보를 key-value로 등록하면 브릿지 하나로 모든
서버에 접근할 수 있다. 이 파일이 존재하면 레거시 환경변수(`MAIA_URL` 등)보다 우선한다.

```json
{
  "defaultServer": "personal",
  "servers": {
    "personal": {
      "url": "%BASE_URL%",
      "apiKeyFile": "~/.maia/personal.key",
      "workspace": "default"
    },
    "enterprise": {
      "url": "https://maia.your-company.example",
      "apiKeyFile": "~/.maia/enterprise.key"
    }
  }
}
```

- `defaultServer`: `server` 인자를 생략한 tool 호출이 향하는 서버. 서버가 2개 이상이면 필수.
- 서버마다 `apiKey` 또는 `apiKeyFile` 중 정확히 하나를 지정한다 (`~/` 경로 지원).
- `workspace`: 그 서버로 향하는 호출의 기본 워크스페이스 (tool 인자가 우선).
- 모든 tool은 선택적 `server` 인자로 대상 alias를 지정할 수 있다.

설정 소스 우선순위 — 첫 번째로 발견된 소스 하나만 사용한다(병합 없음):
`MAIA_SERVERS`(인라인 JSON) → `MAIA_SERVERS_FILE`(파일 경로) → `~/.maia/servers.json`
→ 레거시 환경변수(`MAIA_URL`/`MAIA_API_KEY`/`MAIA_WORKSPACE`).

레지스트리 등록 후 AI 도구 설정에는 env 없이 stdio 서버로만 등록한다:

```json
{
  "mcpServers": {
    "maia": {
      "command": "node",
      "args": ["/절대/경로/maia-mcp/dist/index.js"]
    }
  }
}
```

### 단일 서버 (레거시 환경변수)

서버가 하나뿐이면 레지스트리 파일 없이 환경변수만으로도 동작한다:

```json
{
  "mcpServers": {
    "maia": {
      "command": "node",
      "args": ["/절대/경로/maia-mcp/dist/index.js"],
      "env": {
        "MAIA_URL": "%BASE_URL%",
        "MAIA_API_KEY": "<발급한 키>",
        "MAIA_WORKSPACE": "default"
      }
    }
  }
}
```

Claude Code CLI 한 줄 등록:

```bash
claude mcp add maia -e MAIA_URL=%BASE_URL% -e MAIA_API_KEY=<발급한 키> -- node /절대/경로/maia-mcp/dist/index.js
```

## 6. 동작 확인

브릿지는 기동 직후 stderr에 로드된 서버 목록을 출력한다:

```
[maia-mcp] loaded 2 server(s) from ~/.maia/servers.json: personal (default) → %BASE_URL%, enterprise → https://…
```

AI 도구에서 `search_context`를 호출해 결과가 오는지 확인한다. 문제가 있으면
`%BASE_URL%/health`(연결) → 401 여부(키 유효성) 순으로 점검한다.
"##;

/// 프록시 헤더·환경변수로부터 외부에서 보이는 베이스 URL을 결정한다 (순수 결정 로직).
///
/// 우선순위: 운영자 강제 지정(`MAIA_PUBLIC_URL`) → `X-Forwarded-Proto` + `Host`
/// (리버스 프록시/터널 표준 헤더) → `http://<Host>`. TLS는 항상 프록시에서 종단되므로
/// 소켓 스킴은 판단 근거가 못 된다 — 헤더가 유일한 단서다.
fn derive_base_url(
    env_override: Option<&str>,
    forwarded_proto: Option<&str>,
    host: Option<&str>,
) -> String {
    if let Some(url) = env_override.map(str::trim).filter(|s| !s.is_empty()) {
        return url.trim_end_matches('/').to_string();
    }

    let host = host
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("localhost:8080");
    // X-Forwarded-Proto는 다중 프록시 경유 시 콤마 목록일 수 있다 — 최초(가장 바깥) 값 사용.
    let proto = forwarded_proto
        .and_then(|p| p.split(',').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("http");

    format!("{proto}://{host}")
}

fn public_base_url(headers: &HeaderMap) -> String {
    let env_override = std::env::var("MAIA_PUBLIC_URL").ok();
    derive_base_url(
        env_override.as_deref(),
        headers
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok()),
        headers.get(header::HOST).and_then(|v| v.to_str().ok()),
    )
}

fn repo_url() -> String {
    std::env::var("MAIA_REPO_URL")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_REPO_URL.to_string())
}

fn render_guide(base_url: &str, repo: &str) -> String {
    GUIDE_TEMPLATE
        .replace("%BASE_URL%", base_url)
        .replace("%REPO_URL%", repo)
        .replace("%VERSION%", env!("CARGO_PKG_VERSION"))
}

/// `GET /mcp` — 사람용 설치 안내 (Markdown).
pub async fn mcp_guide_handler(headers: HeaderMap) -> Response {
    let body = render_guide(&public_base_url(&headers), &repo_url());
    (
        [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
        body,
    )
        .into_response()
}

/// `GET /mcp/install.json` — 기계용 설치 매니페스트.
pub async fn mcp_manifest_handler(headers: HeaderMap) -> Json<serde_json::Value> {
    let base = public_base_url(&headers);
    Json(json!({
        "name": "maia-mcp",
        "description": "MCP bridge for Maia (personal knowledge base / RAG server)",
        "transport": "stdio",
        "server": { "base_url": base, "version": env!("CARGO_PKG_VERSION") },
        "requires": { "node": ">=20" },
        "source": {
            "tarball": format!("{base}/mcp/client.tar.gz"),
            "git": { "repo": repo_url(), "subdir": "mcp" }
        },
        "install": ["npm install", "npm run build"],
        "entrypoint": "dist/index.js",
        "config": {
            "servers_file": "~/.maia/servers.json",
            "precedence": ["MAIA_SERVERS", "MAIA_SERVERS_FILE", "~/.maia/servers.json", "legacy env"],
            "env": {
                "MAIA_SERVERS": "inline JSON server registry (highest precedence)",
                "MAIA_SERVERS_FILE": "path to a server registry JSON file",
                "MAIA_URL": "legacy single-server base URL",
                "MAIA_API_KEY": "legacy single-server bearer key",
                "MAIA_WORKSPACE": "legacy single-server default workspace"
            },
            "registry_example": {
                "defaultServer": "personal",
                "servers": {
                    "personal": { "url": base, "apiKeyFile": "~/.maia/personal.key", "workspace": "default" }
                }
            }
        },
        "guide": format!("{base}/mcp")
    }))
}

/// `GET /mcp/client.tar.gz` — 클라이언트 소스 번들.
///
/// 번들은 이미지 빌드 시 고정 경로에 패키징된다. 파일이 없는 배포(로컬 `cargo run` 등)는
/// 404와 함께 git 대안을 안내한다 — 침묵하는 빈 응답보다 명시적 안내가 낫다.
pub async fn mcp_client_tarball_handler() -> Response {
    let path =
        std::env::var("MCP_CLIENT_TARBALL").unwrap_or_else(|_| DEFAULT_TARBALL_PATH.to_string());

    match tokio::fs::read(&path).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/gzip"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"maia-mcp-client.tar.gz\"",
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(err) => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": "MCP client bundle is not packaged in this deployment",
                "detail": err.to_string(),
                "hint": format!("clone {} and use the mcp/ directory instead", repo_url()),
            })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_base_url_env_override_wins() {
        // 운영자 강제 지정이 헤더보다 우선하고, 말미 슬래시는 정규화된다.
        assert_eq!(
            derive_base_url(
                Some("https://maia.example.com/"),
                Some("http"),
                Some("wrong-host"),
            ),
            "https://maia.example.com"
        );
        // 공백뿐인 강제 지정은 무시 — 헤더 경로로 진행.
        assert_eq!(
            derive_base_url(Some("  "), Some("https"), Some("maia.example.com")),
            "https://maia.example.com"
        );
    }

    #[test]
    fn test_derive_base_url_from_proxy_headers() {
        // 터널/리버스 프록시 표준 케이스: X-Forwarded-Proto + Host.
        assert_eq!(
            derive_base_url(None, Some("https"), Some("maia.daybreak.cloud")),
            "https://maia.daybreak.cloud"
        );
        // 다중 프록시 경유 콤마 목록 — 최초 값 사용.
        assert_eq!(
            derive_base_url(None, Some("https, http"), Some("maia.example.com")),
            "https://maia.example.com"
        );
    }

    #[test]
    fn test_derive_base_url_defaults() {
        // 프록시 헤더가 없으면 소켓은 항상 평문이므로 http.
        assert_eq!(
            derive_base_url(None, None, Some("192.168.0.10:9080")),
            "http://192.168.0.10:9080"
        );
        // 아무 단서도 없으면 로컬 기본값.
        assert_eq!(derive_base_url(None, None, None), "http://localhost:8080");
    }

    #[test]
    fn test_render_guide_substitutes_all_placeholders() {
        let md = render_guide("https://maia.example.com", "https://git.example.com/maia");
        assert!(md.contains("https://maia.example.com/mcp/install.json"));
        assert!(md.contains("git clone https://git.example.com/maia.git"));
        // 미치환 플레이스홀더가 남으면 안 된다 — 템플릿·치환 목록의 불일치 감지.
        assert!(!md.contains("%BASE_URL%"));
        assert!(!md.contains("%REPO_URL%"));
        assert!(!md.contains("%VERSION%"));
    }
}
