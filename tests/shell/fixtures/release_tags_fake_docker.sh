#!/usr/bin/env bash
set -euo pipefail

printf '%s\n' "$*" >>"${FAKE_DOCKER_LOG:?}"
case "$*" in
  "buildx imagetools inspect "*)
    if [[ "${FAKE_TRANSIENT_INSPECT:-0}" == 1 && ! -e "${FAKE_TRANSIENT_STATE:?}" ]]; then
      touch "${FAKE_TRANSIENT_STATE}"
      exit 255
    elif [[ "${FAKE_SHA_MISMATCH:-0}" == 1 && "$*" == *":pg17-sha-"* ]] ||
      [[ "${FAKE_PREPARED_MISMATCH:-0}" == 1 && "$*" == *"-prepared"* ]] ||
      [[ "${FAKE_IMMUTABLE_CONFLICT:-0}" == 1 && "$*" == *":pg17-v"* ]] ||
      [[ "${FAKE_POST_MISMATCH:-0}" == 1 && "$*" == *":latest"* ]]; then
      printf 'Digest: sha256:%064d\n' 0
    elif [[ "${FAKE_IMMUTABLE_MISSING:-0}" == 1 && ! -e "${FAKE_DOCKER_STATE:?}" && "$*" != *":pg17-sha-"* && "$*" != *"-prepared"* ]]; then
      echo "manifest unknown" >&2
      exit 1
    else
      printf 'Digest: %s\n' "${FAKE_EXPECTED_DIGEST:?}"
    fi
    ;;
  "buildx imagetools create "*) touch "${FAKE_DOCKER_STATE:?}" ;;
  *) echo "unexpected fake docker command: $*" >&2; exit 1 ;;
esac
