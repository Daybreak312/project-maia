# LLM 프로바이더

> 최종 검증: 2026-07-17 · 기준 커밋: d8862a7 · [문서 인덱스](README.md)

## 추상화 구조 (`backend/src/llm/mod.rs`)

두 개의 trait으로 능력을 분리한다.

- `LlmProvider` — `parse(content) -> ParsedContent`(고정 스키마 파싱) /
  `complete(prompt) -> String`(자유 형식, 에이전트 판단) / `validate_api_key()`
- `EmbeddingProvider` — `embed(text)`(문서) / `embed_query(text)`(쿼리) /
  `dimension()` / `validate_api_key()`

교차 제약은 `ProviderType::valid_for_parsing / valid_for_embedding` 단일 지점에서
판정하며, 위반 시 설정 API가 400을 반환한다.

| Provider | 파싱 | 임베딩 | 인증 수단 |
|----------|:---:|:---:|-----------|
| gemini | ✅ | ✅ (3072d) | API 키 |
| claude | ✅ | ❌ | API 키 **또는** 구독 OAuth 토큰 (자동 감지) |
| openai | ✅ | ✅ (1536d) | API 키 |
| codex | ✅ (전용) | ❌ | ChatGPT 구독 — `auth.json` 임포트 |
| local | ❌ | ✅ (전용, 384d) | 불요 |

파싱/임베딩 provider 선택과 키는 `DATA_DIR/settings.json`에 저장되고
(`backend/src/settings.rs`), Admin UI 또는 설정 API로 바꾼다 (→ [api.md](api.md)).

## Provider별 상세

### Claude (`llm/claude.rs`)

- 파싱 모델: **`claude-sonnet-5`** (2026-07-17 커밋 712484e에서 haiku-4-5 → sonnet-5 전환)
- 엔드포인트: `https://api.anthropic.com/v1/messages` (`anthropic-version: 2023-06-01`)
- **키 접두 자동 감지**:
  - `sk-ant-oat…`(`claude setup-token` 산출물) → OAuth 모드:
    `Authorization: Bearer` + `anthropic-beta: oauth-2025-04-20`, `x-api-key` 제거,
    system 프리픽스 `"You are Claude Code, Anthropic's official CLI for Claude."` 부여
  - 그 외(`sk-ant-api…`) → 기존 `x-api-key` 경로 그대로
- 출력 상한: 파싱 `max_tokens=16384`, complete `8192` — 1024 시절 대형 문서에서
  JSON이 절단돼 "파싱 실패"로 원인이 은폐된 운영 사고(2026-07-17) 이후 상향.
- **절단 가드**: `stop_reason == "max_tokens"`면 성공으로 위장하지 않고 명시적 에러
  (`ensure_not_truncated`).
- **ContentBlock tagged enum**: sonnet-5가 복잡한 입력에서 adaptive thinking 블록
  (`{"type":"thinking",...}`)을 자발 생성한다. `#[serde(tag="type")]` + `#[serde(other)]`로
  비-text 블록을 관용 수용하고 text 블록만 이어붙인다 (thinking 블록이 역직렬화를
  깨뜨리던 두 번째 운영 사고의 수정).
- 주의: thinking 토큰은 출력 예산에 포함된다 — 초대형 문서에서 예산 잠식 가능
  ([known-issues.md](known-issues.md) M6).

### Codex — ChatGPT 구독 (`llm/codex.rs`, 파싱 전용)

- 활성화: `~/.codex/auth.json` 원문을 임포트 (`POST /api/settings/models/codex/import`).
  tokens 중첩/flat 레이아웃 모두 지원, `account_id`는 없으면 id_token JWT에서 유도.
- 파싱: `https://chatgpt.com/backend-api/codex/responses` (Responses API, SSE 관대 집계 —
  알 수 없는 이벤트는 무시).
- **토큰 refresh**: access token의 JWT `exp`가 60초 이내로 임박하면 선제 refresh
  (`https://auth.openai.com/oauth/token`). **단일 플라이트** 게이트로 동시 파싱 호출이
  중복 refresh하지 않고, 회전된 refresh_token은 즉시 영속화. 401이면 강제 refresh 후
  1회 재시도, 실패 시 "재임포트 필요"를 명시한다.
- **비공식 업스트림 게이팅 주의**: ChatGPT Codex 백엔드는 클라이언트 버전으로 모델을
  게이팅한다. 관련 상수는 `llm/codex.rs`의 `upstream` 모듈 **한 곳**에 격리돼 있다
  (출처·검증일 주석 포함) — 드리프트 시 이 모듈만 갱신하면 된다.
  현재 값(2026-07 실측): `MODEL="gpt-5.5"` · `CLIENT_VERSION="0.142.5"` ·
  `ORIGINATOR="codex_cli_rs"` · `OPENAI_BETA="responses=experimental"`.
- 토큰은 로그·Debug·에러에서 앞4+뒤4만 노출(마스킹).

### Local 임베딩 (`llm/local.rs`, 임베딩 전용)

- fastembed `multilingual-e5-small`, **384차원**, 다국어. 외부 키 불요.
- 모델은 `DATA_DIR/models`에 캐시(도커 볼륨 영속) — 첫 embed 호출 시 lazy
  로드/다운로드, 재기동 시 재다운로드 없음.
- **프로세스 전역 모델 캐시** (커밋 d8862a7): provider 인스턴스가 호출 단위로 새로
  조립돼도 같은 캐시 경로면 단일 모델을 공유한다 — 대량 동기화에서 동시 워커 수만큼
  모델(개당 수백 MB)이 중복 로드돼 컨테이너 메모리 캡을 넘기던 OOM 크래시 루프의 수정.
- e5 규약: 문서는 `passage: `, 쿼리는 `query: ` 접두 (`embed` / `embed_query` 분리).
- 초기화·추론은 블로킹 CPU 작업이라 `spawn_blocking`으로 격리.
- 컨테이너 빌드 제약(onnxruntime glibc 2.38+ → debian trixie 필수)은
  [deployment.md](deployment.md) 참조.

### Gemini (`llm/gemini.rs`)

- 파싱 `gemini-2.5-flash` / 임베딩 `gemini-embedding-001` (**3072d**)
- `https://generativelanguage.googleapis.com/v1beta`, 인증은 URL `?key=` 파라미터
- 파싱은 `response_mime_type: application/json` 강제, temperature 0.1

### OpenAI (`llm/openai.rs`)

- 파싱 `gpt-4o-mini` / 임베딩 `text-embedding-3-small` (**1536d**)
- `https://api.openai.com/v1`, `Authorization: Bearer`
- 파싱은 `response_format: json_object` 강제

## 공통 인프라

- **HTTP 타임아웃** (`llm/mod.rs::build_http_client`, 전 provider 단일 지점):
  요청 전체 **300초** / 연결 수립 **10초**. 300초는 thinking 계열 모델이 10KB+ 문서
  파싱에 60초를 초과한 실측(2026-07-07) 반영.
- **재시도**: provider 레벨 재시도는 없다 (예외: codex 401 → refresh 후 1회).
  상위 계층 방어 — Ingest Agent가 판단 재시도 1회, 실패 시 raw 폴백. 429/5xx
  백오프 부재는 [known-issues.md](known-issues.md) M1.
- **공용 파싱 프롬프트** (`build_parse_prompt`): 원본에 없는 내용 추가 금지·해석 금지,
  출력은 `{summary, entities[], facts[]}` JSON. 모든 파싱 provider가 공유한다.
- **응답 후처리** (`extract_json` → `parse_llm_json`): 코드펜스 제거 후 역직렬화.
  산문 포장 응답에 취약한 한계 있음 ([known-issues.md](known-issues.md) M2).

## 임베딩 차원과 마이그레이션

| Provider | 모델 | 차원 |
|----------|------|-----:|
| local | multilingual-e5-small | 384 |
| openai | text-embedding-3-small | 1536 |
| gemini | gemini-embedding-001 | 3072 |

임베딩 provider 확보 시점에 Qdrant `target_dim`이 동기화되고, 컬렉션 실제 차원과
불일치하면 검색·유입이 `embedding dimension mismatch — run POST /api/reindex` 에러를
반환한다(침묵 실패·자동 재생성 없음). `POST /api/reindex`가 컬렉션을 현재 차원으로
재생성하고 raw 전량을 재임베딩해 전환을 완결한다(문서 손실 0). 운영 절차는
[operations.md](operations.md) 참조.
