# Stage 1: Frontend build
FROM node:20-alpine AS frontend

WORKDIR /frontend
COPY frontend/package*.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build

# Stage 2: Backend build
FROM rust:1.83-bookworm AS backend

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY src ./src

RUN cargo build --release

# Stage 3: Runtime
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=backend /app/target/release/maia /usr/local/bin/
COPY --from=frontend /frontend/dist /app/static

ENV SERVER_PORT=8080
ENV DATA_DIR=/data
ENV QDRANT_URL=http://qdrant:6333
ENV STATIC_DIR=/app/static

EXPOSE 8080

CMD ["maia"]
