# Maia — Identity & Goal

## Identity

**"개인 지식 원장이자, 에이전트들이 공유하는 문맥 엔진"**

소유자의 모든 정보를 자연어로 던지면 시스템이 이해하고 정리해 저장하며, 어떤 AI
도구에서든(MCP) 소유자의 문맥으로 쓰이게 한다. 검색 앱이 아니라 **에이전트가 사람처럼
일하게 만드는 메모리 레이어**다.

## Core Value

1. **Zero Friction Input** — 대충 던져도 시스템이 판단해 정리한다 (smart ingest).
2. **기억 유실 0** — 어떤 실패에서도 raw는 보존된다. 이것이 최상위 가치다.
3. **Universal Access** — MCP로 어디서든 접근. REST가 유니버설 인터페이스.
4. **Transparency** — 전략·폴백·출처·탐색 과정이 응답에 드러난다.
5. **Portability** — raw JSON(SSoT) 복사 + reindex만으로 이전 완결.
6. **자립** — 구독(Claude OAuth·Codex) + 로컬 임베딩으로 종량제 키 없이 동작.

## 현재 페이즈

**Phase 1~6 완료, 운영 단계.** 실 배포(Oracle Cloud)에서 소유자의 지식이 커넥터로
지속 유입 중이다. 다음 작업 후보는 견고화 백로그
([docs/known-issues.md](../docs/known-issues.md))에서 고른다 — 착수는 소유자 승인 후.

## 품질 기준

프로토타입이 아니라 **소유자의 기억을 책임지는 시스템**이다. 불변식
([docs/overview.md](../docs/overview.md))을 깨는 변경은 기능이 아니라 사고다.
규칙의 세부는 [policy.md](policy.md), 전체 사실 레퍼런스는 [docs/](../docs/README.md).
