# Maia — 개인 인공두뇌

Maia는 소유자의 모든 정보(일상 기록·프로젝트·관심사·의사결정)를 저장하는 원장이자,
AI 에이전트들(Claude Code, OpenClaw 등)이 공유하는 **문맥 엔진**이다. "문서를 잘 찾는
RAG 앱"이 아니라 **에이전트가 사람처럼 일하도록 문맥을 제공하는 메모리 레이어**를 지향한다.

- **Backend** (`backend/`): Rust + axum. 원장(raw JSON, SSoT) + Qdrant(파생 벡터 인덱스).
- **MCP** (`mcp/`): TypeScript. REST API를 MCP tool로 노출하는 얇은 브릿지.
- **Frontend** (`frontend/`): React + Vite. 관리/검색/브라우즈 UI.

아키텍처 상세는 [`contexts/architecture.md`](contexts/architecture.md) 참조(SSoT).

## 로컬 실행

```bash
# .env에 MAIA_API_KEY 설정(미설정 시 인증 비활성 개발 모드)
docker compose -f docker-compose.local.yml up -d --build
# 앱: http://127.0.0.1:9080  (Qdrant는 내부 네트워크)
```

`cargo test`(backend), `npm run build`(frontend/mcp)가 종료 조건이다.

## 모델 프로바이더 (Phase 6 — 종량제 키 없이 자립)

파싱·임베딩을 소유자의 **구독**과 **로컬 연산**으로 돌려 종량제 API 키 의존을 제거한다.
기존 Gemini/OpenAI provider는 그대로 두고 능력을 추가·전환하는 방식이다.

| 용도 | 선택지 | 활성화 방법 |
|------|--------|-------------|
| 파싱 | gemini / **claude(API키 또는 구독 OAuth)** / openai / **codex(구독)** | 키 등록 또는 auth.json 임포트 |
| 임베딩 | gemini(3072d) / openai(1536d) / **local(384d)** | 키 등록 또는 키 불요(local) |

교차 제약: **local은 임베딩 전용**(파싱 불가), **codex는 파싱 전용**(임베딩 불가).

### 운영 전환 런북 (관리 에이전트 수행)

1. **Codex 활성화** — 터미널에서 `codex login` 후:
   ```bash
   curl -X POST http://127.0.0.1:9080/api/settings/models/codex/import \
     -H "Authorization: Bearer $MAIA_API_KEY" -H "Content-Type: application/json" \
     -d "{\"auth_json\": $(python3 -c 'import json,sys;print(json.dumps(open(sys.argv[1]).read()))' ~/.codex/auth.json)}"
   ```
   또는 Admin UI의 Codex 카드에 `~/.codex/auth.json` 내용을 붙여넣기. 만료된 access
   token은 파싱 호출이 스스로 refresh한다.
2. **Claude 구독 등록**(선택) — `claude setup-token` 실행 → 산출된 `sk-ant-oat…` 토큰을
   Admin UI Claude 카드(또는 `POST /api/settings/models/claude/key`)에 등록. 키 형식이
   자동 감지되어 OAuth 헤더로 전환된다.
3. **파싱/임베딩 provider 선택** — Admin UI에서 파싱=codex(또는 claude), 임베딩=local 선택
   (`PUT /api/settings`).
4. **차원 마이그레이션** — 임베딩을 local(384d)로 바꾸면 UI가 "reindex 필요"를 경고한다.
   `POST /api/reindex` 실행 → 컬렉션이 384d로 재생성되고 전 문서가 재임베딩된다(문서 손실 0).
   reindex 전 검색은 `embedding dimension mismatch — run POST /api/reindex` 에러를 반환한다.
5. **검색 검증** 후 필요하면 gemini 키를 삭제한다. 이 시점부터 종량제 키 없이 구독 둘 +
   로컬 연산만으로 돈다.

**불변식**: 파싱·임베딩·refresh가 전부 실패해도 raw 저장은 성공한다(정보 유실 0).
로컬 임베딩 모델은 `DATA_DIR/models`(도커 볼륨)에 캐시되어 재기동 시 재다운로드하지 않는다.
