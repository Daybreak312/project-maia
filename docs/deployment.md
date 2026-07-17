# 배포

> 최종 검증: 2026-07-17 · 기준 커밋: 712484e · [문서 인덱스](README.md)

## 이미지 빌드 (루트 `Dockerfile`, 3-stage)

| Stage | 베이스 | 내용 |
|-------|--------|------|
| 1 frontend | `node:20-alpine` | `npm ci` → Vite 빌드 → `dist/` |
| 2 backend | `rust:1-trixie` | `cargo build --release` (+ onnx 호환 심) |
| 3 runtime | `debian:trixie-slim` | 바이너리 + `/app/static`(프론트 빌드) + `/data/models` |

**trixie 고정 이유 (변경 금지)**: 로컬 임베딩의 프리빌트 onnxruntime(aarch64)이
glibc 2.38+ 심볼(`__isoc23_*`)을 요구한다 — bookworm(2.36)은 최종 링크가 실패한다.
GCC 12+에서 제거된 `__cxa_call_terminate`는 `backend/docker/onnx_compat.c` 심으로
채우고 `RUSTFLAGS`로 링크에 주입한다. 런타임 의존성: `ca-certificates`(모델 다운로드
TLS) · `libssl3` · `libgomp1` · `libstdc++6`.

## Compose 구성 3종

공통: 서비스 `app`(백엔드) + `qdrant`. Qdrant는 어느 구성에서도 호스트에 포트를 열지
않는다(내부 네트워크 전용). app 환경변수도 공통: `SERVER_PORT=8080` ·
`QDRANT_URL=http://qdrant:6333` · `DATA_DIR=/data` · `STATIC_DIR=/app/static` ·
`MAIA_API_KEY=${MAIA_API_KEY}`.

| | `docker-compose.yml` | `docker-compose.local.yml` | `deploy/oracle/docker-compose.yml` |
|---|---|---|---|
| 용도 | Cloudflare Tunnel 동봉 | 로컬 실행/개발 | Oracle Cloud ARM (현 운영) |
| app 이미지 | `build: .` | `build: .` | `image: project-maia-app:latest` (사전 빌드 이송) |
| app 포트 | 호스트 미노출 (터널이 내부 접속) | `127.0.0.1:9080:8080` | `127.0.0.1:9080:8080` (SSH 터널 전제) |
| 볼륨 | 호스트 경로 (`~/project-maia/…`) | named (`maia-data`, `qdrant-data`) | named (`maia-data`, `qdrant-data`) |
| /knowledge 마운트 | 없음 | 로컬 워크스페이스 ro | 서버 미러 디렉토리 ro |
| 리소스 캡 | 없음 | 없음 | app cpus 2.0 / mem 3g · qdrant cpus 1.0 / mem 1500m · cpu_shares 512 (동거 서비스 우선) |
| 부속 서비스 | `cloudflared` (`CLOUDFLARE_TUNNEL_TOKEN`) | 없음 | 없음 |

`/knowledge/*` ro 마운트는 로컬 디렉토리 커넥터의 스캔 대상이다 — 컨테이너가
소스 파일을 변경할 수 없도록 읽기 전용으로 건다 (→ [ingest.md](ingest.md)).

## 환경변수

루트 `.env.example` 참조. compose가 `.env`에서 치환한다.

| 변수 | 필수 | 의미 |
|------|:---:|------|
| `MAIA_API_KEY` | **사실상 필수** | 마스터 Bearer 키. 미설정이면 인증 전체 비활성(아래 보안 참조) |
| `CLOUDFLARE_TUNNEL_TOKEN` | 터널 구성만 | Cloudflare Zero Trust 터널 토큰 |

백엔드 자체 변수(`SERVER_PORT`/`QDRANT_URL`/`DATA_DIR`/`STATIC_DIR`)는 compose가
고정 주입하므로 보통 만질 일이 없다 (`backend/src/config.rs`).

## 보안 체크리스트

1. **`MAIA_API_KEY`를 반드시 설정한다.** 미설정·빈 값이면 fail-open(전 요청 admin) —
   원격 노출과 결합하면 지식 원장 전체가 공개된다. compose에서
   `MAIA_API_KEY: ${MAIA_API_KEY:?MAIA_API_KEY is required}` 형태의 필수 치환으로
   기동 자체를 막는 방어를 권장 ([known-issues.md](known-issues.md) H1).
2. **포트는 `127.0.0.1`에만 바인딩**하고 외부 접근은 터널(Cloudflare/SSH)을 거친다.
3. 원격 도메인 개통 시 **무인증 프로브로 검증**한다: 키 없이 `/api/metrics`,
   `/api/connectors`, `POST /api/reindex` 호출이 전부 401인지 확인 (2026-07-17
   maia.daybreak.cloud 개통 때의 실측 절차).
4. 에이전트·MCP에는 마스터키 대신 **스코프 키를 발급**해 배포한다 (→ [api.md](api.md)).
5. qdrant 이미지 태그는 현재 `latest`다 — 버전 고정 권장
   ([known-issues.md](known-issues.md) M5).

## 실행 절차

### 로컬

```bash
cp .env.example .env   # MAIA_API_KEY 채우기
docker compose -f docker-compose.local.yml up -d --build
# 앱: http://127.0.0.1:9080 (UI + API)
```

### 원격 서버 (사전 빌드 이송 방식 — Oracle 운영 사례)

레지스트리 없이 이미지를 tar로 이송한다. 서버와 아키텍처가 같아야 한다(둘 다 arm64 등).

```bash
docker build -t project-maia-app:latest .
docker save project-maia-app:latest | gzip > maia-app.tar.gz
scp maia-app.tar.gz <server>:~/ && ssh <server> 'gunzip -c maia-app.tar.gz | docker load'
ssh <server> 'cd ~/maia && docker compose up -d'   # deploy/oracle/docker-compose.yml 사용
```

### 원격 접근 경로

- **SSH 터널**: 클라이언트에서 `ssh -N -L 9080:127.0.0.1:9080 <server>` 상시 유지 →
  로컬 `http://127.0.0.1:9080` 그대로 사용 (MCP/자동화 설정 무변경).
- **Cloudflare Tunnel**: 호스트의 cloudflared ingress에 `<도메인> → localhost:9080`
  규칙 + DNS CNAME. UI는 공개되지만 데이터 접근은 Bearer 키 필수(위 3번 프로브로 검증).

## 데이터 볼륨과 이전

- 진실은 `maia-data` 볼륨(= `DATA_DIR`)이다. `qdrant-data`는 파생 —
  유실돼도 `POST /api/reindex`로 전량 복원된다.
- 서버 이전 = `maia-data` 이송 + 기동 + reindex. 절차는 [operations.md](operations.md).
