# Roadmap & Status — 현황판

> 최종 갱신: 2026-07-17

## 완료된 로드맵

| Phase | 내용 | 완료 근거 |
|-------|------|-----------|
| MVP | Rust 백엔드·하이브리드 검색·MCP·React UI·인증 | git 히스토리 (2026-03) |
| 1 | 기반 완성 — 워크스페이스·API 키·BM25 견고화 | merge 12dd5d5, cargo test 172 |
| 2 | Brain Core — 지식 그래프·Ingest Agent·버전 보관 | dev/phase2 머지 |
| 3 | Search Agent — deep search | dev/phase3 머지 |
| 4 | Connectors — 로컬 디렉토리·스케줄러·대량 적재 | dev/phase4 머지 |
| 5 | Patrol — 탐지기 4종·Review Queue·감쇠·메트릭 | dev/phase5 머지 |
| 6 | 구독 프로바이더(Claude OAuth·Codex)·로컬 임베딩 | merge ec97b1c, cargo test 544 |

상세는 [`prd-maia-brain/`](../prd-maia-brain/00-overview.md), 기능별 현재 동작은
[`docs/`](../docs/README.md).

## 현재 상태 (2026-07-17)

- **운영 단계** — Oracle Cloud ARM에서 docker compose로 상시 구동, OpenClaw
  워크스페이스 지식이 커넥터로 지속 유입, 일일 백업 체계 가동.
- 2026-07-17: 파싱 모델 sonnet-5 전환 핫픽스(712484e), 레포 공개 전환,
  백엔드 전수 감사(→ [docs/known-issues.md](../docs/known-issues.md)),
  문서 체계 개편(docs/ 신설).

## 다음 작업 (대기)

견고화 백로그 — 우선순위와 후보는 [spec.md](spec.md)의 "다음 작업 후보" 참조.
**착수는 소유자 승인 대기 중.**

## 백로그 (기능)

- 버전 복원 UI (스냅샷은 이미 쌓이는 중)
- 그래프/타임라인 시각화
- SaaS 커넥터 (Notion 등 — 로컬 디렉토리 우선 결정으로 보류)
- ChatGPT GPT Actions 등 추가 어댑터 (REST가 유니버설 인터페이스라 얇게 가능)
