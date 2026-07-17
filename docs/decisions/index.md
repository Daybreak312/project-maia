# decisions/ — Architecture Decision Records

> 주요 아키텍처 결정의 이력. "왜 이렇게 되었는가"를 추적한다.
> 새 ADR: [_templates/adr.md](../_templates/adr.md) 복사 → 다음 번호 → 이 표에 추가.

## 운용 규칙

- ADR은 **불변 기록**이다 — 결정이 바뀌면 새 ADR을 쓰고 옛 ADR의 Status를
  `Superseded by ADR-NNN`으로 갱신한다 (부분 대체면 어느 부분인지 명시).
- 현재 사실은 docs/ 본문 문서가 답한다 — ADR은 "왜"의 기록이지 현황 레퍼런스가
  아니다.
- ADR-001~012는 `contexts/decision_log.md`(현 스텁)에서 2026-07-17 이관했다 —
  원문 요약 수준이라 일부 ADR은 Context/Consequences가 얇다. 이관 표시가 하단에 있다.

## ADR 목록

| # | 제목 | 상태 | 날짜 |
|---|------|------|------|
| 001 | [MCP 서버를 TypeScript Thin Wrapper로 구현](001-mcp-typescript-thin-wrapper.md) | Accepted | 2026-03-02 |
| 002 | [모노레포 구조 전환 (backend/ + mcp/ + frontend/)](002-monorepo-structure.md) | Accepted | 2026-03-02 |
| 003 | [API 키 인증 (Bearer Token)](003-api-key-bearer-auth.md) | Accepted | 2026-03-02 |
| 004 | [Maia REST API를 유니버설 인터페이스로](004-rest-universal-interface.md) | Accepted | 2026-03-02 |
| 005 | [Atomic Fact 청킹으로 검색 정밀도 향상](005-atomic-fact-chunking.md) | Accepted | 2026-03-02 |
| 006 | [검색 점수를 Raw Cosine Similarity로 표시](006-raw-cosine-score-display.md) | Accepted | 2026-03-02 |
| 007 | [raw JSON = SSoT, 신규 DB 금지, 무조건 raw 폴백](007-raw-json-ssot.md) | Accepted | 2026-07-06 |
| 008 | [구독 프로바이더 + 로컬 임베딩으로 종량제 탈피](008-subscription-providers-local-embedding.md) | Accepted (파싱 모델은 ADR-010이 개정) | 2026-07-07 |
| 009 | [Oracle Cloud ARM으로 호스팅 이전](009-oracle-arm-hosting.md) | Accepted | 2026-07-08 |
| 010 | [파싱 모델 sonnet-5 전환 + 응답 견고화](010-sonnet5-parsing-hardening.md) | Accepted | 2026-07-17 |
| 011 | [레포 공개 전환](011-repo-public.md) | Accepted | 2026-07-17 |
| 012 | [문서 체계 개편 — docs/ 신설](012-docs-restructure.md) | Accepted | 2026-07-17 |
| 013 | [작업 문서 체계 도입 — ADR·guardrails·prd/exec-plans·templates·green](013-work-docs-system.md) | Accepted | 2026-07-17 |
