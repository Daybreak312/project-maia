# Maia Frontend

React 19 + Vite SPA. 백엔드가 빌드 산출물(`dist/`)을 `STATIC_DIR`로 정적 서빙하며,
API는 상대 경로(`API_BASE=''`)로 같은 오리진을 호출한다.

## 페이지

| 라우트 | 페이지 | 기능 |
|--------|--------|------|
| `/` | Add | 정보 입력(⌘/Ctrl+Enter), 유입 전략 배너, 최근 항목 |
| `/search` | Search | hybrid/vector/keyword 검색, 결과 수정/삭제 |
| `/browse` | Browse | 전체 문서 페이지네이션 |
| `/review` | Review | Patrol 실행·메트릭 카드·Review Queue 판정 |
| `/admin` | Admin | 워크스페이스·API 키·커넥터·모델 설정(키/Codex 임포트/reindex) |

인증 키와 선택 워크스페이스는 localStorage(`maia_auth_key`, `maia_workspace`)에
저장된다 (`src/api/client.ts`).

## 개발

```bash
npm ci
npm run dev      # Vite 개발 서버
npm run build    # tsc -b + vite build → dist/ (종료 조건)
```

전체 문맥은 [../docs/](../docs/README.md) 참조.
