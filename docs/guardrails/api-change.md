# API Change Playbook

## 적용 시점

REST 엔드포인트 추가·변경·삭제, 인증/권한 로직 변경, 요청/응답 스키마 변경.

## 먼저 읽기

- [api.md](../api.md) — 인증·권한 모델, 전체 엔드포인트 현황
- ADR-004 — REST가 유니버설 인터페이스 (모든 어댑터가 이 API를 탄다)

## 절차

1. **현재 계약 확인**: api.md와 라우터(`backend/src/main.rs` + `api/`)에서 기존
   엔드포인트·스키마를 확인한다.
2. **소비자 파악**: 이 계약을 쓰는 클라이언트를 전부 나열한다 —
   MCP(`mcp/src/index.ts`), frontend(`frontend/src/api/`), OpenClaw 등 외부 등록
   클라이언트. **REST 변경은 혼자 끝나지 않는다.**
3. **구현**: 인증 미들웨어를 통과하는 위치에 배치한다. 인증 제외는 `/health`뿐 —
   추가 제외는 만들지 않는다 (H1 fail-open과 결합하면 전면 공개).
4. **어댑터 동기화**: MCP tool이 이 API를 감싸면 `mcp/src/index.ts` 갱신 —
   번역만, 로직 금지 (ADR-001). frontend 호출부도 확인.
5. **문서 갱신**: api.md (+ MCP tool이 바뀌면 mcp.md) 같은 커밋에서.

## 체크리스트

- [ ] 기존 클라이언트(MCP·frontend·외부)와 하위호환인가? 아니면 전부 함께 갱신했는가?
- [ ] 새 엔드포인트가 인증 미들웨어를 통과하는가? (인증 제외 목록에 추가하지 않았는가?)
- [ ] admin 전용 경계(설정·키 관리)가 유지되는가?
- [ ] 응답·로그에 키/토큰 평문이 없는가? (마스킹 앞4+뒤4, 발급 응답 1회만 예외)
- [ ] [definition-of-green.md](../definition-of-green.md) — backend + mcp (+ frontend) 게이트 통과
- [ ] api.md (+ mcp.md) 갱신 (같은 커밋)
