# Maia Phase 3: Agent Layer & Dynamic Knowledge Graph

> 작성: 2026-03-24 / 갱신: 2026-03-30  
> 출처: Daybreak312 아이디어 + 구체화 + RAG 한계 논의

---

## 1. 에이전트 레이어 (검색/저장 종단)

현재 Maia는 **수동 RAG** — 쿼리를 던지면 시스템이 수동적으로 문서를 찾아 반환한다.  
여기에 에이전트를 붙이면 **능동적 지식 탐색**이 가능해진다.

### 1.1 검색 에이전트 (Search Agent)

**핵심 아이디어**: 한 번의 검색으로 끝내지 않고, 결과를 보면서 스스로 쿼리를 조정하며 반복 탐색.

```
User Query
    ↓
Search Agent (LLM)
    ├── 초기 쿼리로 hybrid 검색 실행
    ├── 결과 분석: "이 정도면 충분한가? 누락된 각도가 있는가?"
    ├── 필요시 → 쿼리 재작성 or 새 키워드로 추가 검색
    ├── 그래프 엣지 탐색: 관련 문서 연결 따라가기
    └── 최종 결과 합성 → 컨텍스트 반환
```

**동적 파라미터 조정**:
- 초기 검색 결과가 빈약하면 → 검색 범위(limit) 확대, BM25 비중 증가
- 결과가 너무 산만하면 → score threshold 상향, 그래프 탐색 깊이 줄이기
- 특정 엔티티(사람/회사/기술) 언급 감지 → 해당 엔티티 필터 추가 탐색

**MCP 레벨 변화**: `search_context`가 단순 REST 호출이 아니라 에이전트 루프로 변신.

---

### 1.2 저장 에이전트 (Ingest Agent)

**핵심 아이디어**: 새 정보가 들어올 때 기존 지식 그래프와 비교해서 스마트하게 저장.

```
New Input
    ↓
Ingest Agent (LLM)
    ├── 기존 관련 문서 검색 (유사도 0.7+ 문서 찾기)
    ├── 판단 분기:
    │   ├── [신규 정보] → 새 문서로 저장 + 관련 문서에 엣지 연결
    │   ├── [기존 문서 업데이트] → 기존 문서 내용 병합/수정
    │   └── [중복/상충] → 사용자에게 알림 or 버전 관리
    └── 분할 저장 판단: 입력이 여러 독립 주제 → N개 문서로 분할
```

**분할 저장 로직**:
- LLM이 입력 텍스트의 토픽을 N개로 식별
- 각 토픽이 독립적으로 검색 가능한 단위인지 판단
- "오늘 A회사 면접 봤고 B 라이브러리 공부했다" → 면접 문서 + 학습 문서로 분리

---

## 2. 그래프 기반 문서 저장

현재는 **벡터 공간에 점들이 부유**하는 구조. 여기에 **명시적 관계(엣지)** 를 추가.

### 2.1 그래프 스키마

```
Node: Document
  - id, summary, facts, tags, entities, created_at

Edge: Relation
  - source_id, target_id
  - relation_type: [RELATED_TO | UPDATES | CONTRADICTS | REFERENCES | PART_OF]
  - weight: f32  (관련도, 시간 감쇠 적용)
  - created_at
```

### 2.2 그래프 탐색 전략

검색 시 벡터 유사도로 진입점(entry nodes)을 찾고, 그래프를 BFS/DFS로 확장:

```
깊이(Depth) 조정:
  - depth=1: 직접 연결된 문서만 (빠른 응답)
  - depth=2: 2-hop 연결 포함 (관련 맥락)
  - depth=3+: 전체 연결 클러스터 (심층 리서치)

넓이(Breadth) 조정:
  - 각 노드에서 상위 N개 엣지만 탐색
  - weight + recency 기반으로 탐색 우선순위 결정
```

### 2.3 구현 옵션

| 옵션 | 구현 방식 | 비고 |
|------|-----------|------|
| **Qdrant Payload** | 엣지를 문서 payload에 배열로 저장 | 현재 스택 유지, 간단 |
| **SQLite 그래프** | 별도 `graph.db`에 edges 테이블 | 복잡한 그래프 쿼리 용이 |
| **Neo4j / ArangoDB** | 전용 그래프 DB | 오버엔지니어링 가능성 |

**추천**: 초기엔 Qdrant Payload 방식으로 시작. 복잡도 증가 시 SQLite로 마이그레이션.

---

## 3. 그래프 유지보수 에이전트 (Dynamic Maintenance)

### 핵심 설계 원칙: 반자율(Semi-Autonomous) 접근

> **"완전한 진실 저장소"는 불가능. Maia는 "노이즈 있는 신호 증폭기".**

자율형 관리 에이전트의 근본 문제:
- **외부 ground truth 부재**: RAG 정보로 RAG을 검증하면 자기 참조 순환
- **False positive → 알림 피로**: 잘못된 "이거 오래됐어요" 알림이 쌓이면 시스템 신뢰도 붕괴
- **정보 부패(staleness)는 불가피**: 완전 방지가 아닌 관리가 목표

따라서 **에이전트가 후보 식별 → 사람이 판단 → 피드백으로 정확도 향상 → 점진적 자율화** 경로.

### 3.1 주기적 그래프 순회 (Patrol)

cron job 방식으로 백그라운드에서 실행. **자동 수정이 아닌 플래그 세우기**:

```
Graph Patrol Agent (주기: 매일 새벽)
    ├── Staleness 후보 탐지:
    │   ├── 오래된 문서 (age > threshold) → review_queue에 추가
    │   ├── 외부 소스와 불일치 (Notion 변경됐는데 Maia는 그대로) → 플래그
    │   └── 검색에서 반복 무시되는 문서 (hit 0회, N일 이상) → 플래그
    ├── 고아 노드 탐지: 엣지 없는 문서 → 관련 문서 후보 제안 (자동 연결 X)
    ├── 중복 탐지: 유사도 0.95+ → 병합 후보로 플래그 (자동 병합 X)
    └── 엣지 가중치 재계산: 시간 감쇠 적용 (이건 자동 OK — 판단 아닌 수학)
```

### 3.2 사용자 피드백 기반 학습 (Reactive)

```
사용자 액션 → 시스템 학습:
  - 검색 후 "이 결과 관련 없음" → 해당 문서-쿼리 쌍 negative signal
  - 검색 후 문서 클릭/사용 → positive signal
  - "이 문서 아직 유효해" 확인 → freshness 갱신
  - "이 문서 삭제해" → staleness 패턴 학습
```

프론트엔드에 **"검토 필요(Review Queue)" 대시보드** 추가:
- Patrol이 플래그한 항목 목록
- 각 항목에 "유효/수정필요/삭제" 버튼
- 피드백이 쌓이면 Patrol의 탐지 정확도가 개선됨

### 3.2 성능 분석 & 시각화

**추적 메트릭**:
```json
{
  "search_quality": {
    "avg_score": 0.73,
    "zero_result_rate": 0.05,
    "avg_results_returned": 3.2
  },
  "graph_health": {
    "total_nodes": 342,
    "total_edges": 1205,
    "orphan_nodes": 12,
    "avg_degree": 7.04,
    "largest_cluster_size": 45
  },
  "ingest_stats": {
    "avg_facts_per_doc": 6.3,
    "split_rate": 0.18,
    "update_rate": 0.24
  }
}
```

**시각화**: Force-directed graph (D3.js / Cytoscape.js) — 문서 클러스터, 엣지 강도 시각적 확인.  
React 프론트엔드에 `Graph` 페이지 추가.

**전략 수립**: 분석 결과 기반으로 LLM이 "이런 주제들이 많이 검색되는데 관련 문서가 부족합니다" 같은 인사이트 생성.

---

## 4. 주기적 DB 스크래핑 (외부 소스 자동 수집)

### 4.1 지원 소스

| 소스 | 수집 내용 | 주기 |
|------|-----------|------|
| **Notion** | 지정 DB/페이지의 최신 변경사항 | 매시간 or webhook |
| **Obsidian Vault** | 로컬 마크다운 파일 변경 감지 | 파일 watcher |
| **GitHub** | 스타한 레포, 이슈, PR 코멘트 | 매일 |
| **Readwise** | 하이라이트, 노트 | 매일 |
| **Pocket/Instapaper** | 저장한 아티클 | 매일 |
| **Twitter/X 북마크** | 저장한 트윗 | 매일 |

### 4.2 아키텍처

```
Scraper Service (독립 모듈 or cron job)
    ├── 소스별 커넥터 (Notion API, GitHub API 등)
    ├── 마지막 수집 시각 추적 (cursor-based incremental)
    ├── 중복 방지: URL/ID 기반 dedup
    └── Maia /ingest 엔드포인트로 전달 → 기존 파이프라인 재활용
```

**Notion 특화 설계**:
- OAuth or Integration Token
- Database ID 지정 → 새 항목 or 수정 항목만 가져오기 (`last_edited_time` 필터)
- Page content → 자연어 텍스트로 flatten → Maia ingest

**외부 소스의 이중 가치**:
1. 정보 수집 (기본 목적)
2. **Staleness 검증 시그널** — 외부 DB가 변경됐는데 Maia 관련 문서가 그대로면 → 자기 참조 순환 없이 staleness 탐지 가능. 이게 RAG-검증-RAG 문제의 현실적 해법.

**구현 위치**: `backend/src/scrapers/` 모듈 or 별도 `scraper/` 서비스 (cron 컨테이너)

---

## 5. 내 추가 제안 (즉시 가치 높은 것들)

### 5.1 컨텍스트 감쇠 & 관련도 시간 가중치

오래된 정보가 최신 정보와 동등하게 취급되는 문제 해결:
- 검색 점수에 `time_decay = exp(-λ * days_old)` 적용 옵션
- "최근 3개월" 같은 시간 필터 검색 지원

### 5.2 검색 → 저장 피드백 루프

검색 결과가 0이거나 낮은 점수일 때:
- "이 주제에 대한 정보가 부족합니다. 지금 추가하시겠어요?" 프롬프트
- 외부 검색(Perplexity, Brave API)으로 보완해서 Maia에 자동 저장

### 5.3 문서 버전 관리

현재 PUT `/documents/{id}` 는 덮어쓰기. 변경 히스토리 없음.  
- `document_versions` 테이블 or JSON 파일로 이전 버전 보관
- 지식이 어떻게 발전했는지 추적 가능

### 5.4 워크스페이스 시스템 (핵심 아키텍처)

Maia의 근간은 **워크스페이스 엔진**. Enterprise/Personal은 템플릿(프리셋)일 뿐.

**워크스페이스별 커스터마이징 축:**

```
Workspace Config
├── patrol:
│   ├── frequency: "daily" | "weekly" | custom cron
│   └── strictness: 0.0 ~ 1.0  (느슨 ↔ 엄격)
│
├── connectors:  (외부 소스 — 스크래퍼/MCP)
│   ├── notion: { db_ids: [...], sync_interval: "1h" }
│   ├── github: { repos: [...], sync_interval: "daily" }
│   └── custom_mcp: { server: "...", tools: [...] }
│
├── parsing:
│   ├── entity_priorities: ["service", "team", "api"]  (업무)
│   │                   or ["emotion", "impression"]    (개인)
│   ├── fact_depth: "shallow" | "deep"
│   └── llm_provider: "gemini" | "claude" | "openai"
│
├── search:
│   ├── time_decay_lambda: 0.01  (감쇠 강도)
│   ├── default_mode: "hybrid"
│   └── cross_workspace: ["personal", "work-*"]  (교차 검색 허용 범위)
│
└── templates:
    ├── "personal" → 위 값들 개인용 프리셋
    └── "enterprise" → 업무용 프리셋
    └── 사용자가 모든 값 오버라이드 가능
```

**워크스페이스 단위 접근 제어 (API Key 기반)**:

무거운 RBAC이 아닌, 실용적인 키 기반 접근 관리.

```
data/
├── api_keys.json          # 전체 키 목록
│   [
│     {
│       "key_id": "maia_sk_abc123",
│       "label": "내 맥북",
│       "hashed_key": "sha256:...",
│       "workspaces": ["personal", "work"],    # 접근 가능 워크스페이스
│       "permissions": "read_write",            # read_only | read_write | admin
│       "created_at": "2026-04-01T...",
│       "last_used_at": "2026-04-04T...",
│       "expires_at": null                      # 옵션 — 만료일
│     },
│     {
│       "key_id": "maia_sk_def456",
│       "label": "회사 동료 B",
│       "workspaces": ["work"],                 # work만
│       "permissions": "read_write"
│     }
│   ]
```

핵심 원칙:
- **MAIA_API_KEY (환경변수)** = 마스터 키. 전체 워크스페이스 admin. 기존 호환 유지.
- 워크스페이스별 키 발급 → 기기/사용자별로 다른 키 사용
- Admin UI에서 키 생성/폐기/워크스페이스 바인딩 관리
- 키에 라벨("내 맥북", "아이패드", "동료 B")을 달아서 어떤 기기에서 접근하는지 식별
- `last_used_at` 추적 → 안 쓰이는 키 정리에 활용
- 키 해시만 저장 (평문 노출 방지), 발급 시 한 번만 보여줌

인증 미들웨어 변경:
- 현재: Bearer 토큰 → MAIA_API_KEY 매칭
- 변경: Bearer 토큰 → api_keys.json에서 매칭 → 해당 키의 workspaces 확인 → 요청의 워크스페이스 접근 허용 여부 체크

MCP 연동:
- MCP 클라이언트 설정에서 `MAIA_API_KEY`에 워크스페이스 스코프 키 지정
- 하나의 Maia 서버, 여러 MCP 설정 (기기별로 다른 키/워크스페이스)
```json
// 맥북의 claude_desktop_config.json
{ "env": { "MAIA_API_KEY": "maia_sk_abc123" } }  // personal + work

// 회사 노트북의 claude_desktop_config.json  
{ "env": { "MAIA_API_KEY": "maia_sk_def456" } }  // work only
```

**UI에서 설정 가능** — Admin 페이지 확장. 워크스페이스 생성 시 템플릿 선택 → 세부 설정 조정 → 커넥터 연결까지 한 플로우로.

### 5.5 MCP Tool 확장

에이전트 레이어 완성 시 새 MCP Tool 추가:
- `deep_search`: 그래프 탐색 포함 심층 검색
- `ingest_and_link`: 저장 + 관련 문서 자동 연결
- `get_graph_neighbors`: 특정 문서의 연결 문서 반환
- `knowledge_summary`: 특정 주제의 클러스터 전체 요약

---

## 구현 우선순위 (추천)

| 순위 | 기능 | 이유 |
|------|------|------|
| 🥇 1 | **저장 에이전트 (분할/업데이트 판단)** | 즉시 데이터 품질 향상, 기존 ingest 파이프라인 확장만 필요 |
| 🥈 2 | **Notion 스크래핑** | 자동 수집으로 마찰 제거 — Maia의 핵심 가치 강화 |
| 🥉 3 | **그래프 엣지 (Qdrant Payload 방식)** | 검색 품질 점프, 기존 스택 유지 |
| 4 | **검색 에이전트 (반복 탐색)** | 그래프 완성 후 시너지 최대 |
| 5 | **성능 분석 & 그래프 시각화** | 개발 인사이트 + 멋진 데모 |
| 6 | **시간 감쇠 가중치** | 작은 변경, 큰 검색 품질 향상 |
