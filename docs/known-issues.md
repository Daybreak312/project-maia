# 알려진 이슈와 개선 대기 항목

> 최종 검증: 2026-07-17 · 기준 커밋: 712484e · 출처: 2026-07-17 백엔드 전수 감사(backend/src 20,250줄) · [문서 인덱스](README.md)

수정 전까지 운영자가 알고 있어야 하는 코드 레벨 사실들이다. 항목이 해소되면 이
문서에서 제거하고 관련 문서의 서술을 갱신한다.

## HIGH

### H1. `MAIA_API_KEY` 부재 시 인증 fail-open
- `backend/src/config.rs` + `auth/mod.rs`: 키가 없거나 빈 문자열이면 모든 요청에
  admin 컨텍스트가 주입된다(개발 편의 설계). 원격 노출 배포에서 `.env` 유실·치환 실패
  1회가 곧 전면 공개가 된다.
- 완화: compose 필수 치환(`${MAIA_API_KEY:?...}`) — [deployment.md](deployment.md)
  보안 체크리스트. 근본 해결은 "키 부재 시 기동 거부 + `MAIA_DEV_MODE` opt-in"으로 역전.

### H2. 손상된 `settings.json`의 침묵 초기화
- `backend/src/settings.rs`: 파싱 실패 시 로그 없이 기본값(`api_keys: {}`, provider
  gemini/gemini)으로 대체된다. 등록된 구독 토큰이 메모리에서 사라지고, 다음 save가
  빈 기본값을 디스크에 덮어써 **원본까지 영구 파괴**한다.
- 권고: 파싱 실패 시 `.corrupt` 백업 + 명시적 에러로 기동 중단 —
  같은 코드베이스의 `auth/keys.rs`가 이미 이 패턴을 쓴다(선례 이식).

## MEDIUM

### M1. LLM 429/5xx 재시도 부재 + 키 검증의 오분류
- provider 호출은 비 2xx 즉시 실패(Retry-After 미존중), `validate_api_key`는 429/5xx도
  "무효 키"(false)로 답한다. 대량 재유입 시 일시 장애가 항목 실패로 이월된다.
- 권고: 429/529 한정 지수 백오프 + 검증 3상(유효/무효/일시불가) 분리.

### M2. `extract_json`이 산문 포장 응답에 취약
- `llm/mod.rs`: 코드펜스 trim만 수행 — 선두 산문("다음은 JSON입니다:")이나 후행
  텍스트가 붙으면 serde가 실패한다. 파싱·전략 판단·관계 판단이 전부 이 함수를 지난다.
- 권고: brace matching 또는 선두 JSON 값만 소비하는 파서로 교체 (순수 함수, 테스트 용이).

### M3. 커넥터 poison item — 커서 영구 고착
- 결정적으로 파싱 불가한 항목 1건이 커서 전진을 막아 매 주기 재스캔 + LLM 재호출이
  반복된다(유실은 없음 — 원문 파일 보존). → [ingest.md](ingest.md) 알려진 제약.
- 권고: 항목별 연속 실패 카운트 후 raw 강제 유입 또는 검역(dead-letter) + 커서 전진.

### M4. hybrid 검색의 침묵 강등
- `core/indexer.rs`: 벡터/키워드 한 팔의 실패(Qdrant 순단 등)를 로그조차 없이 남은
  팔로 강등한다. 차원 불일치만 사전 감지로 방어됨. "검색 품질이 왜 나쁘지?"를 진단할
  신호가 없다.
- 권고: 팔 실패 시 `tracing::warn!` + 응답 `degraded: true`.

### M5. `qdrant/qdrant:latest` 미고정
- 세 compose 모두 latest. 운영에서 클라이언트 1.13 vs 서버 1.18 경고 실측.
  `docker compose pull` 한 번에 호환이 깨질 수 있고, 그 증상은 M4에 의해
  "빈 검색 결과"로 위장된다.
- 권고: 서버 버전으로 태그 고정 + 클라이언트 크레이트 업그레이드 계획.

### M6. sonnet-5 thinking의 출력 예산 잠식
- thinking 토큰은 `max_tokens`에 포함된다. 초대형 문서에서 text JSON이 상한 절단되면
  가드가 명시적 에러로 바꿔줄 뿐 성공시키지는 못한다 → 해당 문서는 M3의 poison item이
  된다.
- 권고: 파싱 호출에 thinking 비활성(또는 소예산 고정) 검토.

## LOW

- **L1**: entity 1건의 스키마 위반이 문서 전체 파싱을 무효화 (관용 파싱으로 부분 보존 가능)
- **L2**: 커넥터 sync 상태 로드 실패가 "이력 없음"으로 위장 (`unwrap_or_default`)
- **L3**: 워크스페이스 config 손상 시 목록에서 조용히 제외 (`.corrupt` 백업 권장)
- **L4**: 컨테이너 기동 시 이중 재시작 1회 관찰(2026-07-17, 원인 미확정 — 재발 시
  `docker inspect` State·OOMKilled 확인)
- **L5**: over-fetch 배수(후보 4배·벡터 5배)·한글 접두 32자 상한은 의도된 휴리스틱 타협 —
  코드 주석 참조

## 권고 우선순위

1. H1 — compose 필수 치환 한 줄 (즉시, 무위험)
2. H2 — settings 손상 백업 + 명시 에러 (`auth/keys.rs` 패턴 이식)
3. M5 — qdrant 태그 고정
4. M1·M2·M4 — 프로바이더 견고화 묶음
5. M3·M6 — poison item 정책(dead-letter)과 thinking 제어 (설계 판단 필요)
