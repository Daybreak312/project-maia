# Maia 방향성 정리

## 결론
Maia의 현재 Phase 3 문서 방향은 대체로 옳다. 다만 표현의 중심을 "고급 RAG 시스템"에서 "엔터프라이즈용 에이전트 메모리 및 문맥 인프라"로 조금 더 분명하게 옮기면 좋다.

## 왜 방향이 맞는가
현재 문서들은 이미 아래 요소를 핵심으로 다룬다.
- 에이전트 레이어 기반 검색과 저장
- 문서 간 그래프 관계
- 워크스페이스 분리와 접근 제어
- 외부 소스 커넥터와 지속적 동기화
- Patrol, Review Queue를 통한 반자율 유지보수

이 조합은 단순한 문서 검색 앱이나 RAG 챗봇보다는, AI 에이전트가 조직 안에서 일할 때 사용하는 공용 문맥 시스템에 가깝다.

## 더 선명하게 바꾸면 좋은 점

### 1. 검색 중심 서사를 한 단계 낮추기
현재 문서에는 "검색 품질 향상", "query rewrite", "graph 탐색으로 RAG 개선" 같은 표현이 자주 보인다. 물론 중요하지만, 이들은 정체성 그 자체라기보다 하위 기능이다.

더 적합한 상위 정의는 아래와 같다.
- 조직 지식의 작동 기억(working memory)
- agent-usable enterprise context layer
- shared, governed, evolving memory system

즉, 검색은 목적이 아니라 수단으로 설명되는 편이 더 맞다.

### 2. Document graph에서 Context graph로 확장 가능성을 열어두기
현재 설계는 Document 노드를 중심으로 되어 있고, 이건 출발점으로 좋다. 하지만 엔터프라이즈 문맥은 문서만으로 충분하지 않다.

장기적으로는 아래 단위도 중심 객체가 될 수 있다.
- 사람
- 팀
- 프로젝트
- 고객
- 의사결정
- 회의
- 액션 아이템
- 티켓/이슈
- 시스템 이벤트

따라서 현재는 document-centric graph로 시작하되, 문서 설명에는 장기적으로 work graph 또는 context graph로 확장한다는 관점을 한 줄 넣는 것이 좋다.

### 3. Patrol을 단순 청소가 아니라 memory governance로 설명하기
Patrol은 stale 탐지, orphan 탐지, duplicate 탐지로도 충분히 유용하다. 하지만 Maia의 최종 방향에 더 맞추려면 Patrol은 단순 housekeeping이 아니라 아래 역할도 해야 한다.
- 어떤 지식이 자주 사용되는지 추적
- 어떤 도메인에 문맥 공백이 있는지 파악
- 반복적으로 retrieval 실패가 나는 영역 식별
- 연결 부족, 최신성 부족, 신뢰도 문제를 운영적으로 가시화

즉 Patrol은 maintenance agent이면서 observability 및 governance 계층으로 설명되는 편이 더 적합하다.

## 지금 문서들과의 정합성 평가

### ideas_agent_layer.md
이 문서는 Maia를 living memory system 쪽으로 잘 끌고 간다. 특히 아래가 좋다.
- Search Agent / Ingest Agent 분리
- 그래프 기반 저장
- 반자율 유지보수
- 외부 소스를 staleness 검증 신호로 활용
- 워크스페이스를 핵심 기반으로 둠

이 문서는 큰 방향에서 매우 잘 맞는다.

### phase3_tasks.md
이 문서도 우선순위가 건강하다.
- Workspace를 선행 조건으로 둠
- Ingest Agent와 Graph를 Search Agent보다 먼저 둠
- Connector, Patrol, Review를 별도 운영 계층으로 다룸

이 순서는 엔터프라이즈용 agent context system 구축 순서와 잘 맞는다.

## 최종 판단
Maia는 "문서를 잘 찾는 AI"보다 "AI가 조직 안에서 사람처럼 일할 수 있도록 문맥을 제공하는 시스템" 쪽으로 정의하는 것이 맞다.

현재 문서들은 이미 그 방향으로 많이 가 있다. 따라서 전면 수정이 필요한 것은 아니고, framing을 아래처럼 조금만 더 정리하면 된다.
- agentic RAG system -> enterprise agent memory platform
- better retrieval -> better work execution through context
- document graph -> evolving organizational context graph
- maintenance -> memory health, observability, governance

## 추천 한 줄
Maia는 단순 RAG 앱이 아니라, 엔터프라이즈 AI 에이전트를 위한 공유 문맥 메모리 레이어로 설명하는 편이 가장 정확하다.
