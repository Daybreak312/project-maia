# Policy — Rules & Regulations

> 포맷: `[Category] - [Scope]: [Rule] (Rationale)` — CLAUDE.md 규정.
> 코드를 작성하기 전에 확인하는 법률이다. 위반은 리뷰 반려 사유.

## Security

- `[Security] - [Auth]`: `MAIA_API_KEY` 없이 원격 노출 배포 금지. compose는 필수
  치환(`${MAIA_API_KEY:?...}`)을 지향한다. (이유: 키 부재 = 인증 fail-open, 원장 전면
  공개 — docs/known-issues.md H1)
- `[Security] - [Secrets]`: 키·토큰은 로그/Debug/에러/응답에서 마스킹(앞4+뒤4)한다.
  발급 키 평문은 발급 응답에서 단 1회만 노출. (이유: 로그 유출이 곧 원장 유출)
- `[Security] - [Repo]`: 이 레포는 **공개**다. 실 크리덴셜, 개인 배포 비밀(토큰·계정
  식별자)을 커밋하지 않는다. `.env`는 gitignore. (이유: 2026-07-17 공개 전환)
- `[Security] - [Connector]`: 커넥터는 읽기 전용이며 등록 범위 밖을 읽지 않는다
  (심볼릭 링크 미추종). (이유: 유입 계층이 파일시스템 탈출구가 되면 안 됨)

## Data

- `[Data] - [SSoT]`: raw JSON이 유일한 진실이다. 파생물(Qdrant)은 언제든
  `POST /api/reindex`로 전량 재생성 가능해야 한다. (이유: 이식성·복구 가능성)
- `[Data] - [Loss]`: 어떤 실패 경로도 raw 저장을 막으면 안 된다 — 정보 유실 0.
  LLM·임베딩·refresh 실패는 폴백이지 에러가 아니다. (이유: 기억을 잃으면 안 된다)
- `[Data] - [Delete]`: 자동 삭제 금지. 삭제는 사람의 judge에서만, soft delete(버전
  보관)로. (이유: Patrol 반자율 원칙)
- `[Data] - [Compat]`: raw JSON 스키마 변경은 `#[serde(default)]` / `Option`으로
  하위호환을 유지한다. (이유: 구버전 문서가 영원히 로드 가능해야 함)
- `[Data] - [Write]`: 문서 쓰기(load→수정→save)는 `DocumentStore` write_lock을
  공유한다 — 삭제 포함. (이유: lost-update·삭제 문서 부활 방지)

## Backend

- `[Backend] - [Deps]`: 신규 DB·외부 저장소 도입 금지. 파일 + Qdrant로 해결한다.
  (이유: PRD 킥오프 결정 — 운영 부담·이식성)
- `[Backend] - [LLM]`: LLM 호출 수는 상수 상한. 입력 크기·세그먼트 수 비례 호출 금지.
  (이유: 비용·rate limit 폭주 방지)
- `[Backend] - [Failure]`: 침묵 실패 금지 — 폴백에는 관측 신호(`fallback` 플래그,
  `tracing::warn!`)를 남기고, 차원 불일치류 정합성 문제는 명시적 에러로 올린다.
  (이유: 원인 은폐가 가장 비싼 버그 — 2026-07-17 사고 교훈)
- `[Backend] - [Upstream]`: 비공식 업스트림 의존(코덱스 엔드포인트·버전 게이팅)은
  `llm/codex.rs`의 `upstream` 모듈 한 곳에만 두고 출처·검증일을 주석한다.
  (이유: 드리프트 시 단일 지점 수리)

## Testing

- `[Testing] - [Gate]`: `cargo test` + `npm run build`(frontend/mcp) 통과가 종료 조건.
  버그 수정에는 재발 방지 회귀 테스트를 동반한다.
- `[Testing] - [Isolation]`: 단위 테스트는 Qdrant·실 LLM 무의존 — mock provider,
  trait 주입(`SearchBackend`/`PatrolExecutor`)으로 전 분기를 고정한다.

## Docs

- `[Docs] - [Sync]`: 코드 동작·구조 변경은 같은 커밋/PR에서 `docs/`를 갱신한다.
  매핑표는 [docs/README.md](../docs/README.md). (이유: 문서 부패 방지 — 이 체계의 전제)
- `[Docs] - [Fact]`: 문서에는 코드로 확인한 사실만. 수치·상수는 원문 대조 후 기입.
- `[Docs] - [Decision]`: 아키텍처 갈림길에서의 선택은
  [docs/decisions/](../docs/decisions/index.md)에 ADR로 기록한다. 결정 변경은 새 ADR +
  구 ADR Status 갱신으로. (이유: 결정의 생명주기 추적 — ADR-013)
- `[Docs] - [Guardrail]`: 작업 착수 전 해당 유형의
  [docs/guardrails/](../docs/guardrails/README.md) 체크리스트를 확인한다. 이 정책
  파일이 규칙의 SSoT이고 guardrail은 실행 뷰다 — 충돌 시 여기가 이긴다.
  (이유: 법전과 체크리스트의 역할 분리)
- `[Docs] - [Green]`: 검증 게이트 명령은
  [docs/definition-of-green.md](../docs/definition-of-green.md)에만 산다 — 다른 문서에
  재작성 금지. (이유: 게이트 중복 서술은 곧 불일치)
