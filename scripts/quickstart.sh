#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COMPOSE_FILE="${ROOT}/release/docker/compose.yml"
MODE="${1:-demo}"

require_docker() {
  command -v docker >/dev/null 2>&1 || {
    echo "Docker or Docker Desktop is required" >&2
    exit 1
  }
  docker info >/dev/null 2>&1 || {
    echo "cannot connect to the Docker daemon" >&2
    exit 1
  }
  docker compose version >/dev/null 2>&1 || {
    echo "Docker Compose v2 is required" >&2
    exit 1
  }
}

compose() {
  docker compose -f "${COMPOSE_FILE}" --project-directory "${ROOT}" "$@"
}

wait_for_postgres() {
  for _ in $(seq 1 90); do
    if compose exec -T postgres pg_isready -U postgres -d pgcontext >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  compose logs postgres >&2 || true
  echo "PostgreSQL did not become ready" >&2
  exit 1
}

start() {
  require_docker
  compose up --build -d postgres
  wait_for_postgres
}

case "${MODE}" in
  demo | quickstart)
    start
    compose exec -T postgres psql -U postgres -d pgcontext \
      <"${ROOT}/playground/demo.sql"
    ;;
  setup)
    start
    ;;
  psql)
    start
    compose exec postgres psql -U postgres -d pgcontext
    ;;
  clean)
    require_docker
    compose down --volumes --remove-orphans
    ;;
  package)
    shift
    "${ROOT}/release/build-packages.sh" "$@"
    ;;
  *)
    echo "usage: scripts/quickstart.sh [demo|setup|psql|clean|package]" >&2
    exit 2
    ;;
esac
