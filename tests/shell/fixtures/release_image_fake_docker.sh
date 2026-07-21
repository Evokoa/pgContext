#!/usr/bin/env bash
set -euo pipefail

printf '%s\n' "$*" >>"${FAKE_DOCKER_LOG:?}"
case "${1:-}" in
  load)
    if [[ "${FAKE_LOAD_WRONG:-0}" == 1 ]]; then
      echo "Loaded image: ghcr.io/evokoa/pgcontext:wrong"
    else
      echo "Loaded image: ${FAKE_IMAGE:?}"
    fi
    ;;
  pull) ;;
  run)
    echo fake-container-id
    ;;
  inspect)
    echo healthy
    ;;
  exec)
    command_line="$*"
    case "${command_line}" in
      *"/proc/1/comm"*)
        [[ "${FAKE_NOT_FINAL:-0}" != 1 ]]
        ;;
      *"SHOW server_version_num"*) echo 170010 ;;
      *"string_agg(source_key"*)
        [[ "${FAKE_FILTER_WRONG:-0}" == 1 ]] && echo garden || echo postgres,vectors
        ;;
      *"string_agg(id"*)
        [[ "${FAKE_ORDER_WRONG:-0}" == 1 ]] && echo garden || printf 'SET\npostgres,rust,vectors\n'
        ;;
      *"EXPLAIN (COSTS OFF)"*)
        if [[ "${FAKE_PLAN_WRONG:-0}" == 1 ]]; then
          printf 'SET\nSeq Scan on pgcontext_playground_docs\n'
        else
          printf 'SET\nLimit\n  ->  Index Scan using pgcontext_playground_docs_hnsw on pgcontext_playground_docs\n'
        fi
        ;;
    esac
    ;;
  image | rm) ;;
  *) echo "unexpected fake docker command: $*" >&2; exit 1 ;;
esac
