# Phase 3: 태스크 브레이크다운

> 생성: 2026-04-04  
> 상위 문서: `ideas_agent_layer.md`  
> 상태 범례: ⬜ 미착수 | 🔵 진행중 | ✅ 완료 | ⏸️ 보류

---

## Epic 1: 워크스페이스 시스템 (기반 공사)

모든 Phase 3 기능의 선행 조건. 여기 없으면 나머지 다 공중에 뜸.

### 1.1 워크스페이스 데이터 구조

| # | 태스크 | 상태 | 비고 |
|---|--------|------|------|
| 1.1.1 | `WorkspaceConfig` 구조체 정의 (patrol, parsing, search, connectors 필드) | ⬜ | `backend/src/workspace/config.rs` |
| 1.1.2 | `WorkspaceManager` — config CRUD + 파일 저장/로드 | ⬜ | `data/workspaces/{id}/config.json` |
| 1.1.3 | 템플릿 프리셋 ("personal", "enterprise") | ⬜ | 기본값만 다른 WorkspaceConfig |
| 1.1.4 | 디렉토리 구조 마이그레이션: `data/raw/` → `data/workspaces/default/raw/` | ⬜ | 기존 데이터 default 워크스페이스로 이관 |
| 1.1.5 | `DocumentStore`가 워크스페이스 경로 인식하도록 수정 | ⬜ | 생성자에 workspace_id 파라미터 |

### 1.2 Qdrant 멀티 워크스페이스

| # | 태스크 | 상태 | 비고 |
|---|--------|------|------|
| 1.2.1 | 방식 결정: 컬렉션 분리 vs payload 필터 | ⬜ | 컬렉션 분리 추천 (격리 깔끔) |
| 1.2.2 | `QdrantStorage`에 workspace_id 기반 컬렉션 생성/관리 | ⬜ | `documents_{workspace_id}` |
| 1.2.3 | 교차 워크스페이스 검색 (cross_workspace 설정 기반) | ⬜ | 여러 컬렉션 병렬 검색 → RRF 결합 |

### 1.3 API Key 기반 접근 제어

| # | 태스크 | 상태 | 비고 |
|---|--------|------|------|
| 1.3.1 | `ApiKey` 모델 정의 (key_id, hashed_key, label, workspaces, permissions) | ⬜ | `backend/src/auth/keys.rs` |
| 1.3.2 | `ApiKeyManager` — 키 생성/폐기/조회, `api_keys.json` 관리 | ⬜ | SHA-256 해시 저장, 발급 시 1회 노출 |
| 1.3.3 | `auth.rs` 미들웨어 확장: 키 → 워크스페이스 접근 체크 | ⬜ | 마스터키(MAIA_API_KEY) 하위 호환 |
| 1.3.4 | `last_used_at` 자동 갱신 | ⬜ | 미들웨어에서 비동기 업데이트 |

### 1.4 API 라우트 변경

| # | 태스크 | 상태 | 비고 |
|---|--------|------|------|
| 1.4.1 | 기존 엔드포인트에 `?workspace=` 쿼리 파라미터 추가 | ⬜ | 미지정 시 키의 첫 번째 워크스페이스 |
| 1.4.2 | 워크스페이스 CRUD API: `POST/GET/PUT/DELETE /api/workspaces` | ⬜ | admin 권한만 |
| 1.4.3 | API Key 관리 API: `POST/GET/DELETE /api/keys` | ⬜ | admin 권한만 |

### 1.5 프론트엔드 — 워크스페이스 UI

| # | 태스크 | 상태 | 비고 |
|---|--------|------|------|
| 1.5.1 | Navbar에 워크스페이스 선택 드롭다운 | ⬜ | |
| 1.5.2 | Admin 페이지: 워크스페이스 생성 (템플릿 선택 → 설정 조정) | ⬜ | |
| 1.5.3 | Admin 페이지: API Key 발급/폐기/목록 | ⬜ | |

### 1.6 MCP 변경

| # | 태스크 | 상태 | 비고 |
|---|--------|------|------|
| 1.6.1 | MCP Tool에 `workspace` 파라미터 추가 | ⬜ | 미지정 시 키 기반 기본 워크스페이스 |
| 1.6.2 | `MaiaClient`에 워크스페이스 쿼리 파라미터 전달 | ⬜ | |

**Epic 1 완료 기준**: 워크스페이스 2개 이상 생성, 각각 다른 API 키로 접근, 교차 검색 동작 확인.

---

## Epic 2: 저장 에이전트 (Ingest Agent)

기존 ingest 파이프라인 앞에 에이전트 레이어 삽입.

### 2.1 에이전트 코어

| # | 태스크 | 상태 | 비고 |
|---|--------|------|------|
| 2.1.1 | `IngestAgent` 구조체, `smart_ingest()` 메서드 스캐폴딩 | ⬜ | `backend/src/agent/ingest.rs` |
| 2.1.2 | 판단 프롬프트 설계: 신규/업데이트/분할 판단 | ⬜ | `agent/prompts.rs` |
| 2.1.3 | 분할(Split) 로직: LLM이 토픽 N개 식별 → 각각 ingest | ⬜ | |
| 2.1.4 | 업데이트(Update) 로직: 기존 문서 로드 → 병합 전략 → update | ⬜ | append / replace / merge |
| 2.1.5 | 중복/상충 감지: 유사도 0.9+ → 사용자 알림 or 자동 스킵 | ⬜ | |

### 2.2 API 통합

| # | 태스크 | 상태 | 비고 |
|---|--------|------|------|
| 2.2.1 | `POST /ingest` → `smart_ingest` 호출로 전환 | ⬜ | 기존 직접 ingest는 내부 메서드로 유지 |
| 2.2.2 | 응답에 `strategy: "split"/"update"/"new"` 메타데이터 추가 | ⬜ | |
| 2.2.3 | `POST /ingest?mode=raw` — 에이전트 우회 (디버깅용) | ⬜ | 기존 동작 유지 옵션 |

### 2.3 프론트엔드

| # | 태스크 | 상태 | 비고 |
|---|--------|------|------|
| 2.3.1 | AddPage에 에이전트 판단 결과 표시 ("3개 문서로 분할 저장됨") | ⬜ | |
| 2.3.2 | 에이전트가 업데이트 제안 시 diff 프리뷰 + 확인 UI | ⬜ | 선택적 |

**Epic 2 완료 기준**: "오늘 A회사 면접 봤고 B 라이브러리 공부했다" 입력 시 자동 2개 문서 분할. 유사 문서 존재 시 업데이트 판단.

---

## Epic 3: 그래프 엣지

### 3.1 데이터 모델

| # | 태스크 | 상태 | 비고 |
|---|--------|------|------|
| 3.1.1 | `Edge` 구조체 (target_id, relation_type, weight, created_at) | ⬜ | `models/document.rs` |
| 3.1.2 | `RelationType` enum (RELATED_TO, UPDATES, CONTRADICTS, REFERENCES, PART_OF) | ⬜ | |

### 3.2 Qdrant 저장

| # | 태스크 | 상태 | 비고 |
|---|--------|------|------|
| 3.2.1 | summary chunk payload에 `edges` 배열 추가 | ⬜ | |
| 3.2.2 | `add_edge()` / `remove_edge()` / `get_edges()` 메서드 | ⬜ | `storage/qdrant.rs` |
| 3.2.3 | `get_neighbors(doc_id, depth)` — BFS 그래프 탐색 | ⬜ | depth 제한 필수 |

### 3.3 저장 에이전트 연동

| # | 태스크 | 상태 | 비고 |
|---|--------|------|------|
| 3.3.1 | `smart_ingest`에서 관련 문서 발견 시 자동 `add_edge` | ⬜ | Epic 2 의존 |
| 3.3.2 | 엣지 관계 타입은 LLM이 판단 (UPDATES vs RELATED_TO 등) | ⬜ | |

### 3.4 API

| # | 태스크 | 상태 | 비고 |
|---|--------|------|------|
| 3.4.1 | `GET /documents/{id}/neighbors?depth=1` | ⬜ | |
| 3.4.2 | `POST /documents/{id}/edges` — 수동 엣지 추가 | ⬜ | |

### 3.5 MCP

| # | 태스크 | 상태 | 비고 |
|---|--------|------|------|
| 3.5.1 | `get_graph_neighbors` Tool 추가 | ⬜ | |

**Epic 3 완료 기준**: 문서 저장 시 관련 문서에 자동 엣지 생성. neighbors API로 연결 문서 탐색 가능.

---

## Epic 4: 검색 에이전트 (Search Agent)

Epic 3 (그래프) 선행 필수.

### 4.1 에이전트 코어

| # | 태스크 | 상태 | 비고 |
|---|--------|------|------|
| 4.1.1 | `SearchAgent` 구조체, `deep_search()` 메서드 | ⬜ | `agent/search.rs` |
| 4.1.2 | 검색 품질 평가 프롬프트 ("충분한가? 더 찾아야 하나?") | ⬜ | |
| 4.1.3 | 쿼리 재작성 루프 (max 3회) | ⬜ | 무한 루프 방지 |
| 4.1.4 | 그래프 확장 탐색: 초기 결과 → neighbors 확장 | ⬜ | Epic 3 의존 |
| 4.1.5 | 결과 합성: 멀티 라운드 결과 중복 제거 + 재정렬 | ⬜ | |

### 4.2 API + MCP

| # | 태스크 | 상태 | 비고 |
|---|--------|------|------|
| 4.2.1 | `POST /search?agent=true` — 에이전트 검색 모드 | ⬜ | 기존 검색은 유지 |
| 4.2.2 | `deep_search` MCP Tool 추가 | ⬜ | |

**Epic 4 완료 기준**: "A회사 관련 모든 정보" 쿼리 시, 다각도 탐색 후 관련 문서 클러스터 전체 반환.

---

## Epic 5: 커넥터 시스템

### 5.1 커넥터 프레임워크

| # | 태스크 | 상태 | 비고 |
|---|--------|------|------|
| 5.1.1 | `Connector` trait 정의 (fetch_updates, source_id) | ⬜ | `backend/src/connectors/mod.rs` |
| 5.1.2 | `ConnectorScheduler` — cron 기반 주기 실행 | ⬜ | tokio cron or 별도 태스크 |
| 5.1.3 | 마지막 sync 시각 추적 (`data/workspaces/{id}/sync_state.json`) | ⬜ | |
| 5.1.4 | 중복 방지: source_url/source_id 기반 dedup | ⬜ | Document에 `source` 필드 추가 |

### 5.2 Notion 커넥터 (빌트인)

| # | 태스크 | 상태 | 비고 |
|---|--------|------|------|
| 5.2.1 | Notion API 클라이언트 (Integration Token 기반) | ⬜ | |
| 5.2.2 | DB 쿼리: `last_edited_time` 필터로 incremental fetch | ⬜ | |
| 5.2.3 | Page content → 자연어 flatten → IngestAgent로 전달 | ⬜ | |
| 5.2.4 | 워크스페이스 config에서 Notion DB ID / Token 설정 | ⬜ | |

### 5.3 MCP 커넥터 브릿지

| # | 태스크 | 상태 | 비고 |
|---|--------|------|------|
| 5.3.1 | `McpConnector` — 외부 MCP 서버 Tool을 커넥터로 감싸기 | ⬜ | |
| 5.3.2 | MCP 서버 프로세스 스폰/관리 | ⬜ | STDIO transport |
| 5.3.3 | 워크스페이스 config에서 custom MCP 커넥터 등록 | ⬜ | |

### 5.4 프론트엔드

| # | 태스크 | 상태 | 비고 |
|---|--------|------|------|
| 5.4.1 | Admin: 커넥터 설정 UI (소스 선택, 인증, 주기) | ⬜ | |
| 5.4.2 | Admin: 커넥터 상태 모니터링 (마지막 sync, 에러 로그) | ⬜ | |

**Epic 5 완료 기준**: Notion DB 연결 후 자동으로 새 항목이 Maia에 저장됨. IngestAgent가 분할/병합 판단.

---

## Epic 6: Patrol 에이전트 + Review Queue

Epic 5 (커넥터) 선행 추천 — 외부 시그널이 Patrol 정확도를 높임.

### 6.1 Patrol 코어

| # | 태스크 | 상태 | 비고 |
|---|--------|------|------|
| 6.1.1 | `PatrolAgent` 구조체, `run_patrol()` 메서드 | ⬜ | `agent/patrol.rs` |
| 6.1.2 | Staleness 탐지: age + search hit count 기반 | ⬜ | |
| 6.1.3 | 외부 소스 불일치 탐지: 커넥터 변경 vs Maia 문서 비교 | ⬜ | Epic 5 의존 |
| 6.1.4 | 중복 후보 탐지: 문서 간 유사도 0.95+ | ⬜ | |
| 6.1.5 | 고아 노드 탐지: 엣지 없는 문서 | ⬜ | Epic 3 의존 |
| 6.1.6 | 엣지 시간 감쇠 자동 재계산 | ⬜ | 판단 아닌 수학이라 자동 OK |

### 6.2 Review Queue

| # | 태스크 | 상태 | 비고 |
|---|--------|------|------|
| 6.2.1 | `ReviewItem` 모델 (doc_id, flag_type, reason, status) | ⬜ | |
| 6.2.2 | Review Queue 저장 (`data/workspaces/{id}/review_queue.json`) | ⬜ | |
| 6.2.3 | API: `GET/PUT /api/review` — 큐 조회 + 판단 결과 제출 | ⬜ | |

### 6.3 피드백 학습

| # | 태스크 | 상태 | 비고 |
|---|--------|------|------|
| 6.3.1 | 검색 결과에 "관련 없음" 피드백 버튼 | ⬜ | negative signal |
| 6.3.2 | 피드백 로그 저장 → Patrol 정확도 개선에 활용 | ⬜ | 장기 과제 |

### 6.4 프론트엔드

| # | 태스크 | 상태 | 비고 |
|---|--------|------|------|
| 6.4.1 | Review Queue 대시보드 페이지 | ⬜ | |
| 6.4.2 | 각 항목: 유효/수정필요/삭제 버튼 + 사유 표시 | ⬜ | |

**Epic 6 완료 기준**: Patrol이 주기적으로 실행되어 검토 필요 항목을 대시보드에 노출. 사용자 판단 피드백 저장.

---

## Epic 7: 성능 분석 & 시각화

### 7.1 메트릭 수집

| # | 태스크 | 상태 | 비고 |
|---|--------|------|------|
| 7.1.1 | 검색 로그: 쿼리, 결과 수, 평균 점수, zero-result 여부 | ⬜ | |
| 7.1.2 | Ingest 로그: 전략(split/update/new), facts 수 | ⬜ | |
| 7.1.3 | 그래프 통계: 노드 수, 엣지 수, 고아 노드, 평균 degree | ⬜ | |
| 7.1.4 | 메트릭 저장: `data/workspaces/{id}/metrics/` | ⬜ | daily rollup |

### 7.2 시각화

| # | 태스크 | 상태 | 비고 |
|---|--------|------|------|
| 7.2.1 | 그래프 시각화 페이지 (D3.js or Cytoscape.js) | ⬜ | force-directed layout |
| 7.2.2 | 검색 품질 대시보드 (점수 분포, zero-result 추이) | ⬜ | |
| 7.2.3 | 그래프 헬스 대시보드 (클러스터 크기, 연결도) | ⬜ | |

**Epic 7 완료 기준**: 문서 그래프를 시각적으로 탐색 가능. 검색/저장 품질 추이 확인 가능.

---

## Epic 8: 시간 감쇠 & 검색 개선 (Quick Win)

다른 Epic과 독립적. 언제든 끼워넣기 가능.

| # | 태스크 | 상태 | 비고 |
|---|--------|------|------|
| 8.1 | 검색 점수에 time_decay 옵션: `exp(-λ * days_old)` | ⬜ | 워크스페이스 config의 lambda |
| 8.2 | 시간 필터 검색: `?after=2026-01-01&before=2026-03-31` | ⬜ | Qdrant payload filter |
| 8.3 | 문서 버전 관리: update 시 이전 버전 보관 | ⬜ | `data/workspaces/{id}/versions/` |

---

## 의존 관계 & 추천 실행 순서

```
Epic 1 (워크스페이스) ─────────────────────────────────────────┐
    │                                                          │
    ├──→ Epic 2 (저장 에이전트) ──┐                            │
    │                              ├──→ Epic 4 (검색 에이전트)  │
    ├──→ Epic 3 (그래프 엣지) ────┘                            │
    │                                                          │
    ├──→ Epic 5 (커넥터) ──→ Epic 6 (Patrol)                   │
    │                                                          │
    ├──→ Epic 7 (시각화)  ← Epic 3 이후 가능                   │
    │                                                          │
    └──→ Epic 8 (Quick Win) ← 독립, 아무 때나                  │
```

**추천 순서:**
1. **Epic 1** (워크스페이스) — 기반. 가장 먼저.
2. **Epic 8** (시간 감쇠) — 독립 Quick Win. Epic 1과 병렬 가능.
3. **Epic 2 + 3** (저장 에이전트 + 그래프) — 병렬 진행 가능.
4. **Epic 4** (검색 에이전트) — 2+3 완료 후.
5. **Epic 5** (커넥터) — 4와 병렬 가능.
6. **Epic 6** (Patrol) — 5 이후.
7. **Epic 7** (시각화) — 3 이후 언제든.

---

## 진행 로그

| 날짜 | Epic | 내용 |
|------|------|------|
| 2026-04-04 | — | 태스크 브레이크다운 초안 작성 |
