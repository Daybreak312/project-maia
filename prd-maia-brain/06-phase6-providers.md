# Phase 6: 구독 프로바이더 & 로컬 임베딩 — Gemini 탈피

> **Goal:** 종량제 API 키 없이 두뇌가 돈다 — 파싱은 소유자의 구독(Claude setup-token, ChatGPT Codex OAuth)으로, 임베딩은 로컬 모델로 완전 자립한다.
> **Prerequisite:** Phase 1 (설정/키 저장), Phase 2 (파싱 파이프라인), Phase 4 (커넥터 parsed 유입)
> **Output:** Claude OAuth 지원 + Codex 프로바이더 신규 + 로컬 임베딩 provider + 임베딩 차원 마이그레이션

---

## 1. Purpose (목적)

현재 파싱·임베딩이 Gemini API 키(종량제)에 묶여 있다. 소유자는 이미 Claude 구독과 ChatGPT 구독을 보유하므로, 파싱은 구독 기반 OAuth 토큰으로 옮기고 임베딩은 외부 API 자체를 제거한다(로컬 모델). 결과: 상시 유입 경로(커넥터 시간당 동기화)가 **비용 0·외부 키 의존 0**으로 돌아가는 개인 두뇌.

## 2. Goals (목표)

1. Claude 프로바이더가 `claude setup-token` 산출물(`sk-ant-oat01-…`)을 받아 파싱을 수행한다. 기존 `sk-ant-api…` 키도 계속 동작한다(자동 감지).
2. Codex 프로바이더(신규)가 `~/.codex/auth.json` 임포트로 활성화되고, access token 만료를 스스로 refresh하며 파싱을 수행한다.
3. 로컬 임베딩 provider가 외부 호출 없이 임베딩을 생성한다 (다국어 — 코퍼스가 한국어 중심).
4. 임베딩 프로바이더 전환(차원 변경 포함)이 `POST /api/reindex` 한 번으로 완결된다.
5. Admin UI에서 세 가지 모두 설정·상태 확인·검증이 가능하다.

## 3. Acceptance Criteria (달성 조건)

- [ ] `sk-ant-oat01-…` 토큰 등록 후 parsing_provider=claude로 parsed 유입이 성공한다 (mock 검증 + 헤더 분기 단위 테스트).
- [ ] `sk-ant-api…` 키는 기존 `x-api-key` 경로로 그대로 동작한다 (회귀 테스트).
- [ ] Codex 임포트 API에 auth.json 원문을 넣으면 활성화되고, 만료된 access token은 자동 refresh 후 재시도된다 (mock 상태머신 테스트: 유효→호출 / 만료→refresh→호출 / refresh 실패→명확한 에러).
- [ ] refresh 응답에 회전된 refresh_token이 오면 영속화된다.
- [ ] embedding_provider=local 설정 시 외부 네트워크 호출 없이 임베딩이 생성되고, 모델은 `DATA_DIR/models`에 캐시되어 재기동 후 재다운로드하지 않는다.
- [ ] gemini(3072d) 인덱스 상태에서 local(384d)로 전환 후 `POST /api/reindex` → 컬렉션이 재생성되고 전 문서가 재임베딩되어 검색이 정상 동작한다. 문서 수 손실 0.
- [ ] 차원 불일치 상태(전환 후 reindex 전)의 검색은 "reindex 필요"를 명시하는 에러를 반환한다 (침묵 실패 금지).
- [ ] validation: codex는 embedding_provider로 선택 불가, local은 parsing_provider로 선택 불가 — 400 + 명확한 메시지.
- [ ] 파싱/임베딩이 어떤 방식으로 실패해도 raw 저장은 성공한다 (정보 유실 0 불변식 회귀 테스트).
- [ ] 시크릿(oat 토큰, access/refresh token)이 로그·에러 메시지·API 응답에 원문 노출되지 않는다 (프리뷰는 앞4+뒤4).

## 4. Anti-Goals (하면 안 되는 것)

- Gemini/OpenAI(플랫폼 키) 코드 삭제 — 기존 프로바이더는 그대로 둔다. 이번 phase는 추가와 전환이지 철거가 아니다.
- OAuth 인가 플로우(브라우저 열기, 콜백 서버) 구현 — 토큰 획득은 소유자가 CLI(`claude setup-token`, `codex login`)로 한다. Maia는 받기만 한다.
- 임베딩 모델 선택 UI/멀티 모델 — multilingual-e5-small 하나로 고정. 상위 모델은 백로그.
- 라이브 자격증명을 요구하는 테스트 — 모든 외부 HTTP는 mock. CI에서 구독 토큰 소모 금지.
- 프록시/중계 서버 — Maia가 직접 업스트림을 호출한다.

## 5. User Scenarios

1. **Claude 전환**: 소유자가 터미널에서 `claude setup-token` 실행 → 토큰 복사 → Admin UI Claude 카드에 붙여넣기 → "검증" 클릭(소형 핑 성공) → parsing_provider=claude 선택. 다음 커넥터 동기화부터 Claude가 파싱한다.
2. **Codex 활성화**: 소유자(또는 관리 에이전트)가 `POST /api/settings/models/codex/import`에 `~/.codex/auth.json` 내용을 전달 → 상태 카드에 계정·refresh 시각 표시. 한 달 뒤 access token이 만료돼 있어도 파싱 호출이 스스로 refresh하고 이어간다.
3. **임베딩 자립**: embedding_provider=local 설정 → UI가 "차원 변경 — reindex 필요" 경고 → reindex 실행 → 이후 Gemini 키를 삭제해도 검색·유입이 전부 정상.

## 6. Functional Requirements

### FR1 — Claude OAuth 모드 (기존 provider 확장)
- 키 형식 자동 감지: `sk-ant-oat` 접두 → OAuth 모드, 그 외 → 기존 API 키 모드. 저장·설정 UX는 기존 키 필드 그대로.
- OAuth 모드 요청: `https://api.anthropic.com/v1/messages`, `Authorization: Bearer <token>` + `anthropic-beta: oauth-2025-04-20` (기존 `x-api-key` 헤더 제거). 그 외 요청 본문은 기존과 동일.
- OAuth 토큰이 Claude Code 사용 맥락을 요구해 거절되는 경우를 대비해, 필요 시 붙일 시스템 프리픽스를 상수로 격리하고 실측 결과를 주석으로 남긴다.
- 파싱 모델 상수: `claude-haiku-4-5` (파싱은 구조화 추출 — 속도/쿼터 효율 우선. 상향은 상수 교체 한 줄이어야 함).

### FR2 — Codex 프로바이더 (신규, 파싱 전용)
- 저장 상태: `{access_token, refresh_token, id_token?, account_id, last_refresh}` — 기존 키 저장소와 같은 영속 계층 사용.
- 임포트: `POST /api/settings/models/codex/import` (admin), body = auth.json 원문 JSON. 필수 필드 검증 후 저장. 재임포트는 덮어쓰기.
- Refresh: access_token의 JWT `exp` 임박(여유 60초) 또는 401 응답 시 `POST https://auth.openai.com/oauth/token` `{client_id: "app_EMoamEEZ73f0CkXaXp7hrann", grant_type: "refresh_token", refresh_token, scope: "openid profile email"}` → 갱신분 영속화. 동시 파싱 호출들이 refresh를 중복 실행하지 않도록 직렬화(단일 플라이트).
- 파싱 호출: `POST https://chatgpt.com/backend-api/codex/responses` — Responses API 형태(`instructions` + `input` 메시지 배열), `stream: true`(SSE 집계), `store: false`. 헤더: `Authorization: Bearer`, `chatgpt-account-id: <account_id>`, `OpenAI-Beta: responses=experimental`, `originator: codex_cli_rs`, `session_id: <uuid v4>`.
- SSE 집계는 관대하게: 알 수 없는 이벤트 무시, `response.output_text` 델타 누적, `response.completed`/`response.failed`로 종결. 모델 상수: `gpt-5.1`.
- 업스트림이 비공식임을 전제로 엔드포인트·헤더·client_id를 **단일 상수 모듈**에 격리하고 출처·검증일을 주석으로 남긴다.

### FR3 — 로컬 임베딩 provider (신규, 임베딩 전용)
- fastembed(-rs) 기반 `multilingual-e5-small` (384차원, 다국어). 모델 파일은 `DATA_DIR/models`에 캐시(도커 볼륨 영속).
- lazy 초기화: 첫 embed 호출 시 로드/다운로드. 초기화 실패는 명확한 에러로 — 상위 raw 폴백 불변식은 그대로 발동.
- e5 계열 사용 규약 준수: 문서 임베딩은 `passage: `, 쿼리 임베딩은 `query: ` 접두. (기존 EmbedProvider 인터페이스가 문서/쿼리를 구분하지 않으면 구분하도록 확장.)
- 키 불요: settings에서 local 선택 시 키 검증 스킵. 테스트 엔드포인트는 "모델 로드 + 1회 임베딩 + 차원 확인"으로 대체.

### FR4 — 임베딩 차원 메타 & 마이그레이션
- 각 embedding provider가 차원을 선언한다 (gemini 3072 / openai 1536 / local 384).
- Qdrant 컬렉션 생성·검증 시 현재 provider 차원과 대조. 불일치 = 검색/유입 시 "embedding dimension mismatch — run POST /api/reindex" 에러 (침묵 실패·자동 재생성 금지 — 재생성은 reindex의 명시적 책임).
- `POST /api/reindex`: 차원 불일치 감지 시 컬렉션 drop→현재 차원으로 재생성→raw JSON SSoT 전량 재임베딩. 기존 동작(동일 차원 재인덱싱)은 불변.

### FR5 — 설정·API 확장
- ProviderType에 `codex` 추가. embedding 선택지에 `local` 추가. 교차 선택 validation은 AC 참조.
- `GET /api/settings` 응답 확장: codex는 `{has_auth, account_preview, last_refresh}`, local은 `{model, dim, cache_ready}`.
- 기존 `POST /api/settings/models/{provider}/key`는 claude/gemini/openai 전용 유지. codex는 import 전용.

### FR6 — Admin UI
- Claude 카드: setup-token 안내 문구("터미널에서 `claude setup-token` 실행 후 붙여넣기") + 기존 키 입력 재사용.
- Codex 카드: auth.json 붙여넣기 textarea + 임포트 버튼 + 상태(계정 프리뷰, 마지막 refresh).
- Embedding 카드: local 선택지 + 모델/차원/캐시 상태 + **차원 변경 시 "reindex 필요" 경고와 reindex 실행 버튼**.
- `npm run build` 통과.

## 7. Data Requirements

- codex 토큰 상태는 기존 설정 저장소(JSON, DATA_DIR)와 동일한 위치·직렬화 규약. 마이그레이션: 기존 설정 파일에 codex/local 필드가 없어도 로드가 깨지지 않는다(serde default).
- 문서 raw JSON에는 어떤 변경도 없다 — 임베딩 전환은 파생 인덱스(Qdrant)만 건드린다.

## 8. Constraints & Invariants

- **정보 유실 0 (최상위 불변식)**: 파싱·임베딩·refresh가 전부 죽어도 raw 저장은 성공한다.
- LLM HTTP 전체 타임아웃 300초 유지 — SSE 스트리밍 경로에도 전체 상한이 걸려야 한다(무한 스트림 방지).
- 모든 외부 HTTP는 테스트에서 mock (wiremock 등). 라이브 토큰을 쓰는 테스트 금지.
- 시크릿 로깅 금지. Debug/Display 구현에서 토큰 필드 마스킹.
- 기존 501개 테스트 무회귀.

## 9. Architecture Decision Points (결정 사항)

- **임베딩 차원 3072→384 다운그레이드 수용**: 개인 코퍼스(수십~수천 문서) 규모에서 multilingual-e5-small이면 충분하고, "외부 키 없이 영원히 돈다"는 가치가 검색 정밀도 소폭 손실보다 크다. e5-base/large 승격은 백로그.
- **Codex refresh_token 공유 리스크 수용**: Codex CLI와 같은 refresh_token을 쓴다. OpenAI가 회전 시 재사용을 차단하면 한쪽이 무효화될 수 있음 — 완화: 401+refresh 실패 시 "codex 재임포트 필요"를 상태·에러에 명시. 관찰상 재사용은 허용된다.
- **획득(브라우저 OAuth)은 범위 밖**: 토큰 수명 관리(refresh)까지만 Maia 책임. 획득은 소유자 CLI.
- **gemini-2.5-flash → claude-haiku-4-5 기본 파서 전환은 운영 설정으로**: 코드 기본값은 건드리지 않고, 배포 후 settings로 전환한다 (이 phase의 코드 변경은 능력 추가까지).

## 10. Risks & Mitigations

- **fastembed/onnxruntime이 도커 linux/arm64에서 빌드 실패** → ort download-binaries 지원 확인이 1순위 스파이크. 불가 시 이미지에 런타임 사전 포함(빌드 스테이지 추가). Delivery Checklist에 컨테이너 E2E 항목으로 강제.
- **chatgpt backend 응답 포맷 드리프트** → SSE 파서 관대화 + 상수 모듈 격리 + 실패 시 raw 폴백. 파싱 provider는 언제든 claude로 전환 가능(이중화).
- **모델 최초 다운로드(수백 MB)로 첫 parsed sync 지연** → lazy 로드 + 로그로 다운로드 진행 관측 가능 + 테스트 엔드포인트로 사전 워밍 가능.
- **Claude OAuth가 서버 측 정책으로 거절** → FR1의 프리픽스 상수로 대응하고, 실측 결과를 문서화. 최악의 경우에도 api-key 경로는 살아 있다.

## 11. Delivery Checklist

- [ ] Claude 키 형식 자동 감지 + OAuth 헤더 분기 + api-key 회귀 및 테스트
- [ ] Codex 토큰 저장·임포트 API + JWT exp 파싱 및 테스트
- [ ] Codex refresh 상태머신 (단일 플라이트, 회전 영속화, 실패 에러) 및 mock 테스트
- [ ] Codex responses SSE 클라이언트 + 파싱 경로 및 mock 테스트
- [ ] 로컬 임베딩 provider (multilingual-e5-small, passage/query 접두, DATA_DIR/models 캐시) 및 테스트
- [ ] 임베딩 차원 메타 + 불일치 명시 에러 + reindex 컬렉션 재생성 마이그레이션 및 테스트
- [ ] 설정 확장 (codex/local, 교차 validation, settings 응답, serde 하위호환) 및 테스트
- [ ] 검증 엔드포인트: claude(oat 핑)/codex(refresh 포함)/local(로드+차원) 및 테스트
- [ ] Frontend: Codex 임포트·Claude 안내·local 상태·reindex 경고, `npm run build` 통과
- [ ] 도커 컨테이너 E2E: 이미지 빌드 + local 임베딩 스모크 + `contexts/architecture.md`·README 갱신

## Notes

완료 후 운영 전환(코드 밖, 관리 에이전트 수행): codex auth.json 임포트 → claude setup-token 등록 → parsing=claude·embedding=local 설정 → reindex → 검색 검증 → gemini 키 삭제. 이 시점부터 Maia는 종량제 키 없이 소유자의 구독 두 개와 로컬 연산만으로 돈다.
