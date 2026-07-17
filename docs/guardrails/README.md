# Guardrails — 작업 유형별 체크리스트

> 최종 검증: 2026-07-17 · 기준 커밋: ce73304 · [문서 인덱스](../README.md)

코드에서 derive할 수 없는 운영 위험과 불변 규칙을 작업 유형별 체크리스트로 만든
것이다. **여기 적힌 패턴을 위반하면 컴파일은 되지만 시스템의 존재 이유(기억 유실 0)가
깨진다.** 작업 착수 전, 해당 유형의 문서를 읽는다.

| 작업 유형 | Playbook |
|-----------|----------|
| REST API 추가·변경 | [api-change.md](api-change.md) |
| raw JSON·Qdrant 스키마 변경 | [schema-change.md](schema-change.md) |
| LLM provider·모델·임베딩 변경 | [llm-provider-change.md](llm-provider-change.md) |
| 커넥터 추가·변경 | [connector-change.md](connector-change.md) |
| Dockerfile·compose·배포 구성 변경 | [deploy-change.md](deploy-change.md) |
| 버그 수정 | [bugfix.md](bugfix.md) |

## Dangerous Patterns (금지/주의 패턴)

전 작업 유형 공통. 근거 규칙은 [contexts/policy.md](../../contexts/policy.md),
불변식 원문은 [overview.md](../overview.md).

| 패턴 | 위험 |
|------|------|
| raw 저장 경로에 실패 가능 로직(LLM·임베딩·네트워크) 삽입 | **정보 유실 0 위반** — 그 실패들은 폴백이지 에러가 아니다 |
| Qdrant에만 쓰고 raw JSON에 안 쓰기 | SSoT 위반 — reindex 한 번에 소실되는 데이터 |
| 로그·플래그 없는 침묵 폴백/기본값 대체 | 원인 은폐 — settings 침묵 초기화(H2)·hybrid 침묵 강등(M4)이 실제 전례 |
| `#[serde(default)]`/`Option` 없는 raw 스키마 필드 추가 | 구버전 문서 로드 불가 — 하위호환 파괴 |
| `DocumentStore` write_lock 우회 문서 쓰기 | lost-update, 삭제 문서 부활 |
| MCP 서버(`mcp/`)에 비즈니스 로직 배치 | thin wrapper 계약 위반 (ADR-001) — 로직은 백엔드 REST 뒤에 |
| REST를 우회하는 어댑터(백엔드 내부 직접 접근) | ADR-004 위반 — REST가 유니버설 인터페이스 |
| LLM 호출 수를 입력 크기·세그먼트 수에 비례시키기 | 비용·rate limit 폭주 — 호출 수는 상수 상한 |
| 자동 삭제 로직 (사람 judge 없는 삭제) | Patrol 반자율 원칙 위반 — 삭제는 soft delete + 사람 승인만 |
| compose에 `latest` 이미지 태그 | pull 한 번에 호환 파괴 (M5) — 버전 고정 |

## 변경 시 공통 확인

1. **게이트**: [definition-of-green.md](../definition-of-green.md) — 건드린 컴포넌트의
   게이트 + 보고 규칙(정확한 수치·exit code).
2. **문서 동기화**: [docs/README.md](../README.md) 매핑표 기준, 같은 커밋에서 갱신.
3. **known-issues 확인**: 작업 영역에 [known-issues.md](../known-issues.md) 항목이
   걸려 있는지 — 해소했다면 항목 제거 + 관련 문서 갱신, 악화시킬 수 있다면 명시.
4. **결정 기록**: 아키텍처 갈림길에서 선택했다면 [decisions/](../decisions/index.md)에 ADR.
