# Stage 1: Frontend build
FROM node:20-alpine AS frontend

WORKDIR /frontend
COPY frontend/package*.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build

# Stage 2: Backend build
# 최신 stable Rust를 쓴다 — fastembed 5.x/ort 2.0(로컬 임베딩)이 요구하는 MSRV가
# 높아, 구 버전 핀(1.86)에서 빌드가 깨질 수 있다.
FROM rust:1-bookworm AS backend

# native-tls(모델 다운로드용 openssl) 빌드 헤더. ort download-binaries는 빌드시
# 프리빌트 onnxruntime을 내려받아 **정적 링크**하므로 런타임 .so는 불필요하다.
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY backend/Cargo.toml backend/Cargo.lock* ./
COPY backend/src ./src

RUN cargo build --release

# Stage 3: Runtime
FROM debian:bookworm-slim

# 런타임 의존성:
# - ca-certificates: HuggingFace 모델 다운로드 TLS 신뢰.
# - libssl3: native-tls(모델 다운로드)의 openssl 동적 링크.
# - libgomp1: onnxruntime(정적 링크)이 요구하는 OpenMP 런타임.
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    libgomp1 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=backend /app/target/release/maia /usr/local/bin/
COPY --from=frontend /frontend/dist /app/static

ENV SERVER_PORT=8080
ENV DATA_DIR=/data
ENV QDRANT_URL=http://qdrant:6333
ENV STATIC_DIR=/app/static

# 로컬 임베딩 모델 캐시 위치(DATA_DIR/models). /data는 볼륨으로 영속되어
# 재기동 시 재다운로드하지 않는다.
RUN mkdir -p /data/models

EXPOSE 8080

CMD ["maia"]
