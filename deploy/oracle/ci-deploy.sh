#!/usr/bin/env bash
# CI 배포 스크립트 — GitHub Actions가 forced-command SSH로 호출한다.
# 설치 위치: 서버 /home/ubuntu/maia/ci-deploy.sh
# authorized_keys 항목(배포 전용 키):
#   restrict,command="/home/ubuntu/maia/ci-deploy.sh" ssh-ed25519 <pubkey> maia-ci-deploy
#
# 입력(stdin 한 줄): "<GHCR_TOKEN> <IMAGE_REF>"
#   - 토큰을 인자가 아닌 stdin으로 받는 이유: 인자는 ps 목록에 노출된다.
#   - IMAGE_REF는 이 레포의 GHCR 이미지 sha 태그만 허용한다.
set -euo pipefail

read -r TOKEN IMAGE_REF
if ! [[ "$IMAGE_REF" =~ ^ghcr\.io/daybreak312/project-maia-app:sha-[0-9a-f]{7,40}$ ]]; then
  echo "reject: invalid image ref: $IMAGE_REF" >&2
  exit 1
fi

echo "$TOKEN" | docker login ghcr.io -u ci --password-stdin >/dev/null
trap 'docker logout ghcr.io >/dev/null 2>&1 || true' EXIT
docker pull "$IMAGE_REF"

# 롤백 포인트: 교체 직전 latest의 이미지 ID를 기억한다.
PREV_ID="$(docker inspect --format '{{.Id}}' project-maia-app:latest 2>/dev/null || true)"

docker tag "$IMAGE_REF" project-maia-app:latest
cd "$HOME/maia"
docker compose up -d app

# 헬스체크: 최대 60초 대기, 실패 시 이전 이미지로 자동 롤백.
for _ in $(seq 1 30); do
  sleep 2
  if curl -sf -m 3 http://127.0.0.1:9080/health >/dev/null; then
    echo "deploy OK: $IMAGE_REF"
    # 이하 부수 작업 — 실패해도 배포 성공에 영향 없음.
    # 소스 미러(~/maia/src) 동기화
    (cd "$HOME/maia/src" && git fetch -q origin && git checkout -q main && git merge -q --ff-only origin/main) || true
    # GHCR sha 태그 정리: 최신 2개(현재+직전 롤백 포인트)만 유지
    docker images ghcr.io/daybreak312/project-maia-app --format '{{.Tag}}' \
      | grep '^sha-' | tail -n +3 \
      | xargs -r -n1 -I{} docker rmi "ghcr.io/daybreak312/project-maia-app:{}" >/dev/null 2>&1 || true
    docker image prune -f >/dev/null 2>&1 || true
    exit 0
  fi
done

echo "health check FAILED — rolling back" >&2
docker logs --tail 40 maia-app-1 >&2 || true
if [ -n "$PREV_ID" ]; then
  docker tag "$PREV_ID" project-maia-app:latest
  docker compose up -d app
  echo "rolled back to previous image" >&2
fi
exit 1
