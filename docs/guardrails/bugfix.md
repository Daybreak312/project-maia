# Bugfix Playbook

## 적용 시점

버그 수정, 장애 대응, 예상과 다른 동작의 수정.

## 먼저 읽기

- [operations.md](../operations.md) 트러블슈팅 표 — 이미 알려진 증상인지
- [known-issues.md](../known-issues.md) — 원인이 이미 목록에 있는 항목인지

## 절차

1. **재현 또는 위치 특정**: 로그·코드·테스트로 실패 행동을 확인한다. 추측으로
   고치지 않는다 — root cause를 코드/로그로 100% 특정한 뒤 움직인다.
2. **침묵 실패 의심**: 마이아의 실제 사고 패턴은 "증상이 원인을 위장"이다 —
   설정 침묵 초기화(H2), hybrid 침묵 강등(M4), sync 이력 위장(L2), latest 태그
   호환 파괴가 "빈 검색 결과"로 위장(M5). 보이는 증상 뒤에 침묵 폴백이 있는지 먼저
   의심한다.
3. **경로 추적**: 요청 흐름(api → core → storage/llm/connectors)을 따라간다.
4. **회귀 테스트 고정**: 수정 전에 실패하는 테스트를 먼저 작성한다.
5. **최소 범위 수정**: root cause를 해결하는 가장 작은 변경. 시스템을 뒤엎지 않는다.
6. **관측 신호 동반**: 침묵 실패를 고치면서 또 다른 침묵을 만들지 않는다 —
   폴백에는 `tracing::warn!`·플래그를 남긴다 (`policy.md` `[Backend] - [Failure]`).
7. **검증**: 좁게(해당 테스트) → 넓게([definition-of-green.md](../definition-of-green.md)).
8. **후처리**: known-issues 항목을 해소했으면 제거 + 관련 문서 서술 갱신.
   새 함정을 발견했으면 known-issues에 추가.

## 체크리스트

- [ ] root cause가 코드/로그로 특정되었는가? (증상 수리 아님)
- [ ] 수정 전에 실패하던 회귀 테스트가 추가되었는가?
- [ ] 폴백·에러 경로에 관측 신호가 남는가? (침묵 실패 금지)
- [ ] raw 데이터 보존이 전 실패 경로에서 유지되는가?
- [ ] [definition-of-green.md](../definition-of-green.md) 게이트 + 정확한 수치 보고
- [ ] known-issues.md 정리 + 관련 문서 갱신 (같은 커밋)
