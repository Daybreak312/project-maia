# PRD Self-Review — 2026-07-06

## 점검 결과

| 항목 | 판정 | 비고 |
|------|------|------|
| AC 테스트 가능성 | ✅ | 전 phase AC가 관측 가능한 동작 단위. LLM 판단 품질은 "구조·안전장치 완성"으로 기준 명확화 (P2 Notes) |
| Anti-Goals 충분성 | ✅ | 전역(overview) + phase별. 특히 신규 DB 금지, 자동 삭제 금지, 실 LLM 테스트 금지 명시 |
| 시나리오 예외 흐름 | ✅ | LLM 장애 폴백(P2/P3), 파일 실패 격리(P4), 오탐 상한(P5) 포함 |
| Data Requirements | ✅ | 관계·의미 수준 유지, 스키마 상세는 자율 영역 |
| Phase 의존성 | ✅ | P2→P3, P2→P4, (P2,P3,P4)→P5. P1은 전체 선행 |
| Constraints 구현 가능성 | ✅ | "reindex 엣지 복원"은 엣지의 raw JSON 저장 결정과 정합 |
| HOW 침투 | ✅ | 파일 경로 언급은 Technical Context(면제 영역)에 한정. 구현 선택은 Architecture Decision Points의 자율 항목으로 위임 |
| Checklist ↔ AC 매핑 | ✅ | 각 phase 체크리스트가 AC 상위 집합 |

## 원 설계 문서 대비 범위 조정 (근거 포함)

- **Epic 8(시간 감쇠/버전)을 Phase 2에 흡수** — 업데이트 경로가 버전 보관을 필요로 해 자연 결합.
- **Notion 커넥터 → 백로그, 로컬 디렉토리 커넥터를 레퍼런스로** — 소유자의 실지식은 로컬 마크다운(OpenClaw 워크스페이스)에 있음. "모든 정보를 쏟아붓는" 목표에 직결.
- **그래프 시각화(Epic 7 일부) → 백로그** — 두뇌 기능과 무관한 데모성. 메트릭 수치는 P5에 유지.
- **MCP 커넥터 브릿지 → 백로그** — 실사용 요구 없음.
- **Patrol 탐지기는 LLM 없이 신호 기반** — 비용·오탐 제어. 원 설계의 반자율 원칙과 일치.

## 잔여 리스크

- dev agent가 Qdrant 통합 테스트 환경을 과도하게 요구할 수 있음 → Constraints에 env-guard 명시로 완화.
- Phase 4 대량 적재의 LLM 비용 — mode=raw 옵션과 동시성 제한으로 통제, 실행 시점은 운영 판단.
