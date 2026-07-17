# Schema Change Playbook

## 적용 시점

raw JSON 문서 스키마(`backend/src/models/`) 변경, Qdrant 컬렉션·payload 구조 변경,
디스크 레이아웃(`storage/`) 변경, 청킹 구조 변경.

## 먼저 읽기

- [data-model.md](../data-model.md) — Document/Edge 구조, 디스크 레이아웃, Qdrant
  스키마, 쓰기 직렬화, reindex
- ADR-007 — raw JSON = SSoT, 신규 DB 금지

## 절차

1. **현재 구조 확인**: data-model.md + `models/`·`storage/` 원문 대조.
2. **하위호환 설계**: raw JSON 필드 추가는 `#[serde(default)]` 또는 `Option`.
   기존 필드의 의미 변경·삭제는 구버전 문서가 영원히 로드 가능해야 한다는 제약
   아래에서만 — 마이그레이션 스크립트보다 "읽을 때 관용" 우선.
3. **쓰기 경로 확인**: 문서를 쓰는 모든 경로(load→수정→save)가 `DocumentStore`
   write_lock을 공유하는지 — 삭제 포함.
4. **파생 인덱스 정합**: Qdrant payload가 바뀌면 reindex(`POST /api/reindex`)가
   신구 문서 모두에서 멱등하게 재생성하는지 확인.
5. **문서 갱신**: data-model.md 같은 커밋에서.

## 체크리스트

- [ ] 구버전 raw JSON 로드 테스트가 있는가? (스키마 변경마다 회귀 케이스 추가)
- [ ] 새 필드에 `#[serde(default)]`/`Option`이 붙었는가?
- [ ] raw에 먼저 쓰고 Qdrant는 파생으로 따라오는가? (역순 금지)
- [ ] reindex가 이 변경 후에도 전량 재생성 가능한가?
- [ ] 신규 DB·외부 저장소를 도입하지 않았는가? (파일 + Qdrant로 해결)
- [ ] write_lock 경유가 유지되는가?
- [ ] [definition-of-green.md](../definition-of-green.md) — backend 게이트 통과
- [ ] data-model.md 갱신 (같은 커밋)
