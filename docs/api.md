# REST API 레퍼런스

> 최종 검증: 2026-07-17 · 기준 커밋: 712484e (`backend/src/main.rs` 라우터 대조) · [문서 인덱스](README.md)

## 인증

모든 엔드포인트(단 `/health` 제외)는 `Authorization: Bearer <token>`을 요구한다.
`require_api_key` 미들웨어(`backend/src/auth/mod.rs`)가 순서대로 해석한다:

1. **마스터키** — 환경변수 `MAIA_API_KEY`와 상수 시간 비교 → 전체 admin
2. **등록 키** — SHA-256 해시 조회 + 만료 확인 → 키에 부여된 스코프
3. 불일치 → **401**

> ⚠️ **fail-open 주의**: `MAIA_API_KEY`가 미설정(또는 빈 문자열)이면 인증이 통째로
> 비활성화되어 모든 요청이 admin으로 통과한다(개발 모드). 원격 노출된 배포에서 이
> 상태는 곧 전면 공개다 — 반드시 [deployment.md](deployment.md)의 보안 체크리스트를
> 따를 것.

### 권한 3단계

| 권한 | 읽기 | 쓰기 | 워크스페이스·키·설정 관리 |
|------|:---:|:---:|:---:|
| `read_only` | ✅ | ❌ | ❌ |
| `read_write` | ✅ | ✅ | ❌ |
| `admin` | ✅ | ✅ | ✅ |

### 발급 키의 워크스페이스 스코프 (fail-closed)

영속 키는 `workspaces[]`에 명시된 워크스페이스에만 접근한다. 빈 목록은 "전체"가
아니라 **접근 없음**이며, 발급 시 최소 1개를 요구한다(400). "unscoped = all"은
마스터키 전용 의미다. 평문 키는 발급 응답에서 단 1회만 노출된다.

## 공통 규약

- **`?workspace={id}`** — 문서/검색/유입/커넥터/Patrol 엔드포인트 공통. 미지정 시 키에
  바인딩된 기본 워크스페이스(마스터·개발 모드는 `default`). 미존재 404, 접근 불가 403.
- 요청/응답 본문은 JSON. 삭제 계열은 204 No Content.
- 주요 에러: 401(인증 실패) · 403(`Admin/Write permission required`, 워크스페이스 접근
  거부) · 404(워크스페이스/문서 없음) · 400(유효성) · 409(커넥터 중복 실행) · 500(내부).

## 문서와 검색

| METHOD | 경로 | 권한 | 설명 |
|--------|------|------|------|
| POST | `/ingest?workspace=&mode=` | write | 유입. 기본 smart ingest, `mode=raw`는 에이전트 우회. 응답 `IngestOutcome` |
| POST | `/search?workspace=` | read | 검색. 응답 `SearchResponse` |
| GET | `/documents/{id}?workspace=` | read | 원문 조회 (raw_content·entities·source 포함) |
| PUT | `/documents/{id}?workspace=` | write | 내용 교체 — 재파싱 + 재임베딩, 이전 버전 보관 |
| DELETE | `/documents/{id}?workspace=` | write | 삭제 (raw + 전체 청크), 204 |
| GET | `/recent?limit=&offset=&workspace=` | read | 최근 문서 목록 (기본 limit 20) |
| POST | `/api/reindex?workspace=` | write | 전량 재인덱싱. 응답 `{indexed}` → [operations.md](operations.md) |

`POST /ingest` body: `{"content": "..."}` ·
`POST /search` body:

```json
{
  "query": "필수",
  "mode": "hybrid | vector | keyword",
  "limit": 5,
  "agent": true,          // deep search opt-in
  "time_decay": true,     // 순위 최신 가중 opt-in
  "since": "RFC3339", "until": "RFC3339",
  "cross_workspace": true // 워크스페이스 설정의 교차 목록 사용
}
```

## 그래프

| METHOD | 경로 | 권한 | 설명 |
|--------|------|------|------|
| GET | `/documents/{id}/neighbors?depth=&workspace=` | read | BFS 이웃 (depth 1~5, 상한 200) |
| POST | `/documents/{id}/edges?workspace=` | write | 수동 엣지 추가 `{target, relation, weight?}` |
| DELETE | `/documents/{id}/edges/{target}?workspace=` | write | 엣지 제거 |

## 설정과 모델 키 (`backend/src/api/settings.rs`)

| METHOD | 경로 | 권한 | 설명 |
|--------|------|------|------|
| GET | `/api/settings` | read | provider 목록·선택 상태·codex/local 상태 (키는 앞4+뒤4 마스킹) |
| PUT | `/api/settings` | admin | 파싱/임베딩 provider 선택 (교차 제약 위반·미설정 provider는 400) |
| POST | `/api/settings/models/{provider}/key` | admin | API 키 등록 (claude/gemini/openai — codex/local은 400) |
| DELETE | `/api/settings/models/{provider}/key` | admin | 키 삭제 |
| POST | `/api/settings/models/{provider}/test` | admin | 키 검증. `local`은 모델 로드 + 임베딩 1회로 검증 |
| POST | `/api/settings/models/codex/import` | admin | `{"auth_json": "<~/.codex/auth.json 원문>"}` 임포트 |

## API 키 관리 (`backend/src/api/keys.rs`)

| METHOD | 경로 | 권한 | 설명 |
|--------|------|------|------|
| GET | `/api/keys` | admin | 키 목록 (해시 미노출 뷰) |
| POST | `/api/keys` | admin | 발급 `{label, workspaces[], permissions?, expires_at?}` → 응답에 평문 키 1회 |
| DELETE | `/api/keys/{key_id}` | admin | 폐기, 204 |

## 워크스페이스 (`backend/src/api/workspaces.rs`)

| METHOD | 경로 | 권한 | 설명 |
|--------|------|------|------|
| GET | `/api/workspaces` | admin | 전체 목록 |
| GET | `/api/workspaces/{id}` | admin | 단일 조회 |
| POST | `/api/workspaces` | admin | 생성 `{id, name, template?}` |
| DELETE | `/api/workspaces/{id}` | admin | 삭제 (`default` 불가, 문서·컬렉션·설정 best-effort 정리) |

## 커넥터 (`backend/src/api/connectors.rs`)

| METHOD | 경로 | 권한 | 설명 |
|--------|------|------|------|
| GET | `/api/connectors?workspace=` | read | 목록 + 동기화 상태 (+실행 중이면 진행률) |
| GET | `/api/connectors/{id}/status?workspace=` | read | 단일 상태 |
| POST | `/api/connectors?workspace=` | admin | 등록 (`ConnectorInstance` — 중복 id는 400), 201 |
| DELETE | `/api/connectors/{id}?workspace=` | admin | 등록 해제 (유입 문서는 보존), 204 |
| POST | `/api/connectors/{id}/sync?workspace=` | admin | 즉시 실행 `{mode?, full?, concurrency?}` → 202 (백그라운드), 실행 중이면 409 |

## Patrol · 거버넌스 (`backend/src/api/patrol.rs`)

| METHOD | 경로 | 권한 | 설명 |
|--------|------|------|------|
| POST | `/api/patrol/run?workspace=` | write | 동기 실행 → `PatrolRun` 리포트 |
| GET | `/api/patrol/history?workspace=` | read | 실행 이력 |
| GET | `/api/review?workspace=&status=&kind=` | read | Review Queue 조회 |
| POST | `/api/review/judge?workspace=` | write | 판단 `{ids[], decision}` (멱등, 단건·일괄 통합) |
| POST | `/api/feedback?workspace=` | write | 검색 결과 "관련 없음" `{query, document_id}`, 204 |
| GET | `/api/metrics?workspace=&from=&until=` | read | 일자 롤업 (기본 최근 30일) |

## 기타

| METHOD | 경로 | 권한 | 설명 |
|--------|------|------|------|
| GET | `/health` | 없음 | `"OK"` 평문. 모니터링용 |
| GET | (그 외 경로) | 없음 | `STATIC_DIR` 정적 서빙 fallback (프론트엔드 SPA) |

- CORS: 전체 허용 (origin/method/header Any).
- 서버는 `0.0.0.0:{SERVER_PORT}`(기본 8080)에 바인딩 — 외부 노출 제어는 compose의
  포트 매핑에서 한다 (→ [deployment.md](deployment.md)).

## 예시

```bash
# 검색 (deep search)
curl -s -X POST "$MAIA_URL/search" \
  -H "Authorization: Bearer $MAIA_API_KEY" -H "Content-Type: application/json" \
  -d '{"query": "오라클 이전 결정 배경", "agent": true}'

# 유입
curl -s -X POST "$MAIA_URL/ingest" \
  -H "Authorization: Bearer $MAIA_API_KEY" -H "Content-Type: application/json" \
  -d '{"content": "2026-07-17 maia.daybreak.cloud 개통. Bearer 키 필수."}'

# 커넥터 수동 동기화 (파싱 모드, 증분)
curl -s -X POST "$MAIA_URL/api/connectors/<id>/sync?workspace=default" \
  -H "Authorization: Bearer $MAIA_API_KEY" -H "Content-Type: application/json" \
  -d '{"mode": "parsed", "full": false}'
```
