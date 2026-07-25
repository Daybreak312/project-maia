# Stage 1: Frontend build
FROM node:20-alpine AS frontend

WORKDIR /frontend
COPY frontend/package*.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build

# Stage 2: Backend build
# **trixie(Debian 13, glibc 2.41)를 쓴다.** 로컬 임베딩용 프리빌트 onnxruntime
# (aarch64)은 최신 툴체인(glibc 2.38+)으로 빌드돼 C23 심볼 `__isoc23_strtol`/
# `__isoc23_strtoll`/`__isoc23_strtoull`(glibc 2.38+)과 `__libc_single_threaded`
# (glibc 2.32+)를 참조한다. bookworm(glibc 2.36)은 isoc23 계열이 없어 최종 링크가
# `undefined reference`로 깨진다(실측 2026-07-07). trixie(2.41)는 이 심볼을 전부
# 제공한다. 남는 건 libstdc++ 심볼 `__cxa_call_terminate`(GCC12+에서 제거) 하나뿐이라
# 아래 호환 심(onnx_compat.c)으로 채운다. (macOS 로컬 빌드는 libc++라 무관.)
FROM rust:1-trixie AS backend

# native-tls(reqwest·모델 다운로드용 openssl) 빌드 헤더. ort download-binaries는
# 빌드시 프리빌트 onnxruntime을 내려받아 정적 링크한다(libstdc++는 동적 링크 → 런타임 필요).
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY backend/Cargo.toml backend/Cargo.lock* ./
COPY backend/src ./src

# onnxruntime <-> 최신 libstdc++ 호환 심. 프리빌트 onnxruntime(GCC<=11 빌드)이
# 참조하는 `__cxa_call_terminate`가 GCC12+에서 제거돼 trixie libstdc++(GCC14)에도
# 없어서 생기는 undefined reference를 채운다(상세 근거는 onnx_compat.c 주석). -fPIC로
# 컴파일해 Rust의 PIE 바이너리 링크와 충돌하지 않게 한다.
COPY backend/docker/onnx_compat.c ./onnx_compat.c
RUN gcc -O2 -fPIC -c onnx_compat.c -o /opt/onnx_compat.o

# 심 오브젝트를 최종 바이너리 링크에 주입해 onnxruntime.a의 미해결 참조를 해소한다.
# (RUSTFLAGS는 모든 링크 단계에 적용되지만, 심은 그 외 바이너리에서 미참조로 무해하다.)
ENV RUSTFLAGS="-Clink-arg=/opt/onnx_compat.o"
RUN cargo build --release

# Stage 3: Runtime
# 빌더와 동일 계열(trixie)이라 libstdc++6/libssl3/glibc 버전이 정확히 맞는다.
FROM debian:trixie-slim

# 런타임 의존성:
# - ca-certificates: HuggingFace 모델 다운로드 TLS 신뢰.
# - libssl3: native-tls(모델 다운로드)의 openssl 동적 링크.
# - libgomp1: onnxruntime(정적 링크)이 요구하는 OpenMP 런타임.
# - libstdc++6: onnxruntime C++ 코드가 동적으로 참조하는 libstdc++.
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    libgomp1 \
    libstdc++6 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=backend /app/target/release/maia /usr/local/bin/
COPY --from=frontend /frontend/dist /app/static

# MCP 클라이언트 소스 번들 — `GET /mcp/client.tar.gz`로 서빙된다 (api/mcp.rs).
# 온프레미스 환경이 외부 저장소 접근 없이 서버에서 직접 브릿지를 받게 하기 위함.
COPY mcp/package.json mcp/package-lock.json mcp/tsconfig.json /tmp/mcp-pack/maia-mcp/
COPY mcp/src /tmp/mcp-pack/maia-mcp/src
RUN tar -czf /app/mcp-client.tar.gz -C /tmp/mcp-pack maia-mcp && rm -rf /tmp/mcp-pack

ENV SERVER_PORT=8080
ENV DATA_DIR=/data
ENV QDRANT_URL=http://qdrant:6333
ENV STATIC_DIR=/app/static

# 로컬 임베딩 모델 캐시 위치(DATA_DIR/models). /data는 볼륨으로 영속되어
# 재기동 시 재다운로드하지 않는다.
RUN mkdir -p /data/models

EXPOSE 8080

CMD ["maia"]
