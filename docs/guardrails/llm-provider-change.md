# LLM Provider Change Playbook

## 적용 시점

provider 추가·제거, 파싱/임베딩 모델 변경, 타임아웃·max_tokens 조정,
키/토큰 저장 로직 변경.

## 먼저 읽기

- [llm-providers.md](../llm-providers.md) — provider 5종, 임베딩 차원 표, 타임아웃,
  OAuth/토큰 시맨틱
- ADR-008 (구독+로컬 자립) · ADR-010 (sonnet-5 전환과 응답 견고화 — 사고 이력 포함)

## 절차

1. **현재 상수 확인**: 모델명·차원·타임아웃·max_tokens를 코드 원문에서 확인한다
   (문서 수치는 대조용 — 코드가 진실).
2. **provider 배선**: 새 provider는 `backend/src/llm/` 구현 + `ProviderType` 분기
   (`valid_for_*`, 팩토리) + 설정 API 연결까지가 한 세트.
3. **임베딩 차원 영향**: 임베딩 모델이 바뀌면 차원 불일치는 **명시적 에러**로
   올라가야 한다 (침묵 강등 금지). 기존 인덱스와 차원이 다르면 reindex 필요 —
   운영 절차는 [operations.md](../operations.md).
4. **응답 견고성**: 파싱 응답은 thinking 블록이 섞일 수 있다(ContentBlock tagged
   enum 관용 수용 유지). `stop_reason=max_tokens` 절단 가드를 우회하지 않는다.
   thinking 토큰이 `max_tokens` 예산을 잠식하는 문제(M6)를 인지하고 조정한다.
5. **settings 저장**: `settings.rs`를 만지면 H2(손상 시 침묵 초기화 → save가 원본
   파괴)를 악화시키지 않는지 확인 — 백업+명시 에러 패턴(`auth/keys.rs` 선례) 지향.
6. **비공식 업스트림**: Codex류 비공식 의존은 `llm/codex.rs`의 `upstream` 모듈
   한 곳에만 — 출처·검증일 주석 (`policy.md` `[Backend] - [Upstream]`).
7. **문서 갱신**: llm-providers.md의 수치·표 (+ 전환 절차가 바뀌면 operations.md).

## 체크리스트

- [ ] LLM 호출 수가 여전히 상수 상한인가? (입력 크기 비례 호출 금지)
- [ ] 실패 시 raw 저장이 보존되는가? (파싱 실패 = 폴백, 유입 차단 아님)
- [ ] 429/5xx를 "무효 키"로 오분류하는 경로(M1)를 새로 만들지 않았는가?
- [ ] 차원 변경 시 명시 에러 + reindex 경로가 있는가?
- [ ] 키·토큰이 로그/응답에서 마스킹되는가?
- [ ] 타임아웃(300초 — 대형 문서 실측 근거)을 줄이지 않았는가? 줄인다면 근거는?
- [ ] [definition-of-green.md](../definition-of-green.md) — backend 게이트 통과
- [ ] llm-providers.md 수치 갱신 (같은 커밋)
