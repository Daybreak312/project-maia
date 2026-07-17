# Connector Change Playbook

## 적용 시점

커넥터 타입 추가, 커서/sync 로직 변경, 스케줄러 변경, 유입 파이프라인 변경.

## 먼저 읽기

- [ingest.md](../ingest.md) — 유입 파이프라인, Ingest Agent 전략, 커서 시맨틱,
  알려진 제약
- `policy.md` `[Security] - [Connector]` — 읽기 전용·범위 제한

## 절차

1. **커서 시맨틱 확인**: ingest.md에서 커서 전진 조건·재스캔 동작을 확인한다.
2. **커넥터 배선**: 새 타입은 `Connector` trait 구현 + `build_connector` 팩토리 +
   `ConnectorSpec` variant가 한 세트.
3. **실패 경로 설계**: 항목 1건의 실패가 커서를 영구히 막는 poison item(M3)을
   인지한다 — 새 커넥터가 같은 함정을 복제하지 않게. 어떤 실패에서도 원문(raw)
   보존이 우선.
4. **범위 제한**: 등록 범위 밖 읽기 금지, 심볼릭 링크 미추종, 쓰기 금지.
5. **문서 갱신**: ingest.md (+ 커서 시맨틱이 바뀌면 알려진 제약 절도).

## 체크리스트

- [ ] 커서가 전진하는 조건이 명확한가? 실패 항목이 커서를 영구히 막지 않는가?
- [ ] 파싱 실패 항목도 raw로는 보존되는가? (정보 유실 0)
- [ ] 커넥터가 등록 범위 밖을 읽지 않는가? (심볼릭 링크 포함)
- [ ] 대량 재유입 시 LLM 호출이 폭주하지 않는가? (M1 재시도 부재와 결합 위험)
- [ ] sync 상태 로드 실패가 "이력 없음"으로 위장(L2)되는 경로를 새로 만들지 않았는가?
- [ ] [definition-of-green.md](../definition-of-green.md) — backend 게이트 통과
- [ ] ingest.md 갱신 (같은 커밋)
