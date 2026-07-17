# 운영 런북

> 최종 검증: 2026-07-17 · 기준 커밋: d8862a7 · [문서 인덱스](README.md)

## 상태 확인

```bash
curl -s http://127.0.0.1:9080/health                     # "OK" (인증 불요)
curl -s -H "Authorization: Bearer $KEY" \
  "http://127.0.0.1:9080/api/metrics?workspace=default"  # 일자 롤업 (검색/그래프/유입/Patrol)
docker compose logs -f app                               # tracing 로그 (기본 maia=info)
```

## 파싱/임베딩 프로바이더 전환 런북

종량제 키 없이 구독 + 로컬 연산으로 돌리는 표준 절차 (관리 에이전트 수행 가능).

1. **Codex 활성화** — 터미널에서 `codex login` 후:
   ```bash
   curl -X POST $MAIA_URL/api/settings/models/codex/import \
     -H "Authorization: Bearer $MAIA_API_KEY" -H "Content-Type: application/json" \
     -d "{\"auth_json\": $(python3 -c 'import json,sys;print(json.dumps(open(sys.argv[1]).read()))' ~/.codex/auth.json)}"
   ```
   또는 Admin UI Codex 카드에 `~/.codex/auth.json` 내용을 붙여넣기. 만료된 access
   token은 파싱 호출이 스스로 refresh한다.
2. **Claude 구독 등록**(선택) — `claude setup-token` → `sk-ant-oat…` 토큰을 Admin UI
   Claude 카드(또는 `POST /api/settings/models/claude/key`)에 등록. 접두가 자동 감지돼
   OAuth 헤더로 전환된다.
3. **provider 선택** — Admin UI 또는 `PUT /api/settings`로 파싱=codex(또는 claude),
   임베딩=local.
4. **차원 마이그레이션** — 임베딩 차원이 바뀌면(예: 3072→384) UI가 "reindex 필요"를
   경고하고, 검색은 `embedding dimension mismatch — run POST /api/reindex` 에러를 낸다.
   `POST /api/reindex` 실행 → 컬렉션 재생성 + 전 문서 재임베딩 (문서 손실 0).
5. **검색 검증** 후 필요하면 잔여 종량제 키를 삭제한다.

## reindex — 언제, 어떻게

```bash
curl -X POST -H "Authorization: Bearer $KEY" \
  "$MAIA_URL/api/reindex?workspace=default"    # 응답: {"indexed": N}
```

실행하는 경우: ① 임베딩 provider/차원 전환 ② `qdrant-data` 유실·재생성
③ 백업 복구 직후 ④ 인덱스 정합성 의심. raw JSON(SSoT)에서 그래프 엣지까지 전량
복원되며, 도중 삭제된 문서는 부활하지 않는다 (→ [data-model.md](data-model.md)).
워크스페이스가 여럿이면 워크스페이스별로 호출한다.

## 커넥터 운영

```bash
# 상태(마지막 실행·커서·실패 목록·실시간 진행)
curl -s -H "Authorization: Bearer $KEY" "$MAIA_URL/api/connectors?workspace=default"

# 수동 동기화 — 증분
curl -X POST -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  "$MAIA_URL/api/connectors/<id>/sync?workspace=default" -d '{"mode":"parsed","full":false}'

# 전량 재처리 (커서 무시) / 쿼터 보호 대량 적재
... -d '{"mode":"parsed","full":true}'
... -d '{"mode":"raw","full":true}'
```

- 202가 돌아오면 백그라운드 실행 중 — 진행은 상태 API로 관측. 실행 중 재트리거는 409.
- **커서는 실패 0일 때만 전진**한다. `failed`가 계속 남으면 같은 항목이 매 주기
  재시도되고 있다는 뜻 — 실패 목록(source_id)을 확인하고, 결정적 실패(poison item)면
  해당 파일을 수정/제외하거나 `mode=raw`로 한 번 흘려보낸다
  ([known-issues.md](known-issues.md) M3).

## 백업과 복구

**백업 대상 = `DATA_DIR` (maia-data 볼륨)** — 그중에서도 `workspaces/`(문서·버전·
거버넌스·커서), `api_keys.json`, `settings.json`. `models/`(재다운로드 가능)와
`qdrant-data`(reindex로 재생성)는 제외해도 된다.

```bash
# 백업 (모델 캐시 제외)
docker run --rm -v maia_maia-data:/data -v "$PWD":/out debian:trixie-slim \
  tar czf /out/maia-raw-$(date +%F).tar.gz -C /data --exclude=models .

# 복구: 볼륨에 풀고 → 기동 → reindex
docker run --rm -v maia_maia-data:/data -v "$PWD":/in debian:trixie-slim \
  tar xzf /in/maia-raw-<날짜>.tar.gz -C /data
docker compose up -d
curl -X POST -H "Authorization: Bearer $KEY" "$MAIA_URL/api/reindex?workspace=default"
```

서버 이전도 동일 원리다: `workspaces/` + 두 JSON을 옮기고 reindex. `settings.json`은
LLM 키를 포함하므로 백업 파일 자체를 안전하게 다뤄야 한다(평문 저장).

## 트러블슈팅

| 증상 | 원인 | 조치 |
|------|------|------|
| 검색/유입이 `embedding dimension mismatch` | 임베딩 provider 차원 ≠ 컬렉션 차원 | `POST /api/reindex` (에러 메시지 그대로) |
| 401 전면 발생 | 키 불일치·만료 | 마스터키 확인, `GET /api/keys`로 만료 확인 |
| **모든 요청이 무인증으로 통과** | `MAIA_API_KEY` 미설정 → fail-open | 즉시 키 설정 후 재기동. [deployment.md](deployment.md) 보안 체크리스트 |
| 파싱이 계속 실패, raw 폴백만 쌓임 | provider 키/토큰 문제 또는 절단 | `POST .../test`로 키 검증, 로그에서 `max_tokens`(절단 가드)·401(재임포트 필요) 확인 |
| codex 파싱 401/거부 | access token 만료+refresh 실패, 또는 업스트림 버전 게이팅 드리프트 | auth.json 재임포트. 게이팅이면 `llm/codex.rs` `upstream` 모듈 갱신 (→ [llm-providers.md](llm-providers.md)) |
| 검색 품질 급락(빈 결과 아님) | hybrid 한 팔(벡터/키워드) 침묵 강등 가능성 | Qdrant 컨테이너 상태·로그 확인 ([known-issues.md](known-issues.md) M4 — 관측 신호 부재) |
| qdrant 로그에 client/server 버전 경고 | 이미지 `latest`와 클라이언트 크레이트 버전 차 | 경고 자체는 무해. 태그 고정 권장 (M5) |
| settings.json이 초기화된 듯 (provider가 gemini로 돌아감) | 파일 손상 시 침묵 기본값 대체 (H2) | 즉시 서버 정지 → 파일 검사/복구 → 키 재등록. 백업에서 복원 |
| 컨테이너가 임베딩 로드 중 OOM/재시작 루프 | 모델 동시 중복 로드 — d8862a7(전역 캐시)로 수정됨 | 최신 이미지 재배포 확인. 그래도 재발하면 `docker inspect`로 OOMKilled 확인 후 `concurrency` 하향 |

## 관측 팁

- 유입 결과의 `fallback: true` 비율이 올라가면 파싱 계층이 아프다는 신호다 —
  `GET /api/metrics`의 유입 전략 분포로 추세를 본다.
- 검색 zero-result율(메트릭 롤업)은 인덱스 정합성·차원 문제의 조기 신호다.
- Patrol Review Queue가 비정상적으로 쌓이면 strictness 설정과 탐지기별 분포를 본다
  (→ [patrol.md](patrol.md)).
