#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"

out_dir="${RELEASE_ARTIFACT_REPORT_DIR:-${REPO_ROOT}/target/release-artifacts/$(git -C "${REPO_ROOT}" rev-parse HEAD)}"
dry_run=0
allow_dirty=0
require_signatures=0
artifacts=()
repo_physical="$(cd -P "${REPO_ROOT}" && pwd -P)"

usage() {
  cat <<'USAGE'
Usage: scripts/run-release-artifact-report.sh [options]

Write an auditable release-artifact checksum/signature report.

Options:
  --artifact PATH        Release artifact to record. May be repeated.
  --out-dir PATH         Report directory. Defaults under target/release-artifacts.
  --require-signatures   Keep approval incomplete unless every artifact has
                         PATH.asc, PATH.sig, or PATH.minisig next to it.
  --allow-dirty          Run on a dirty worktree, but keep approval incomplete.
  --dry-run              Write the report plan without requiring artifact files.
  -h, --help             Show this help text.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --artifact)
      [[ $# -ge 2 ]] || {
        echo "--artifact requires a value" >&2
        exit 2
      }
      artifacts+=("$2")
      shift 2
      ;;
    --out-dir)
      [[ $# -ge 2 ]] || {
        echo "--out-dir requires a value" >&2
        exit 2
      }
      out_dir="$2"
      shift 2
      ;;
    --require-signatures)
      require_signatures=1
      shift
      ;;
    --allow-dirty)
      allow_dirty=1
      shift
      ;;
    --dry-run)
      dry_run=1
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -z "${out_dir}" || "${out_dir}" == "/" ]]; then
  echo "--out-dir must be a non-root path" >&2
  exit 2
fi
if [[ "${out_dir}" != /* ]]; then
  out_dir="${REPO_ROOT}/${out_dir}"
fi

if [[ ${#artifacts[@]} -eq 0 ]]; then
  echo "at least one --artifact is required" >&2
  exit 2
fi

for ((i = 0; i < ${#artifacts[@]}; i++)); do
  for ((j = i + 1; j < ${#artifacts[@]}; j++)); do
    if [[ "${artifacts[$i]}" == "${artifacts[$j]}" ]]; then
      echo "duplicate artifact: ${artifacts[$i]}" >&2
      exit 2
    fi
  done
done

artifact_path() {
  local artifact="$1"
  if [[ "${artifact}" == /* ]]; then
    printf '%s\n' "${artifact}"
  else
    printf '%s/%s\n' "${REPO_ROOT}" "${artifact}"
  fi
}

artifact_label() {
  local artifact="$1"
  local absolute="$2"
  if [[ "${artifact}" == /* ]]; then
    repo_relative_path "${absolute}"
  else
    printf '%s\n' "${artifact}"
  fi
}

repo_relative_path() {
  local path="$1"

  case "${path}" in
    "${REPO_ROOT}"/*) printf '%s\n' "${path#"${REPO_ROOT}/"}" ;;
    "${repo_physical}"/*) printf '%s\n' "${path#"${repo_physical}/"}" ;;
    *) printf '%s\n' "${path}" ;;
  esac
}

sha256_file() {
  local path="$1"
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "${path}" | awk '{print $1}'
  else
    sha256sum "${path}" | awk '{print $1}'
  fi
}

file_size() {
  local path="$1"
  if stat -f '%z' "${path}" >/dev/null 2>&1; then
    stat -f '%z' "${path}"
  else
    stat -c '%s' "${path}"
  fi
}

cargo_pgrx_version_matches() {
  local actual="$1"
  local expected="$2"
  local actual_version

  case "${actual}" in
    "cargo-pgrx "*) ;;
    *) return 1 ;;
  esac
  actual_version="${actual#cargo-pgrx }"
  actual_version="${actual_version%%[[:space:]]*}"
  [[ "${actual_version}" == "${expected}" ]]
}

reject_external_artifact_path() {
  local path="$1"
  local parent
  local parent_physical

  parent="$(dirname "${path}")"
  if ! parent_physical="$(cd -P "${parent}" 2>/dev/null && pwd -P)"; then
    return 0
  fi
  case "${parent_physical}" in
    "${repo_physical}" | "${repo_physical}"/*) return 0 ;;
    *) return 1 ;;
  esac
}

signature_for() {
  local path="$1"
  local candidate
  for candidate in "${path}.asc" "${path}.sig" "${path}.minisig"; do
    if [[ -f "${candidate}" ]]; then
      printf '%s\n' "${candidate}"
      return 0
    fi
  done
  return 1
}

verify_signature() {
  local artifact="$1"
  local signature="$2"

  case "${signature}" in
    *.minisig)
      command -v minisign >/dev/null 2>&1 || return 1
      minisign -Vm "${artifact}" -x "${signature}" >/dev/null 2>&1
      ;;
    *.asc | *.sig)
      command -v gpg >/dev/null 2>&1 || return 1
      gpg --batch --verify "${signature}" "${artifact}" >/dev/null 2>&1
      ;;
    *)
      return 1
      ;;
  esac
}

generation_log_for() {
  local path="$1"
  local candidate

  for candidate in "${path}.build.log" "${path}.generation.log"; do
    if [[ -f "${candidate}" ]]; then
      printf '%s\n' "${candidate}"
      return 0
    fi
  done
  return 1
}

first_toml_value() {
  local file="$1"
  local key="$2"
  awk -F'=' -v key="${key}" '
    $1 ~ "^[[:space:]]*" key "[[:space:]]*$" {
      value = $2
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
      gsub(/^"|"$/, "", value)
      print value
      exit
    }
  ' "${file}"
}

metadata_value() {
  local key="$1"
  awk -F'=' -v key="${key}" '
    /^\[workspace.metadata.pgcontext\]$/ { in_metadata = 1; next }
    /^\[/ && in_metadata { exit }
    in_metadata && $1 ~ "^[[:space:]]*" key "[[:space:]]*$" {
      value = $2
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
      gsub(/^"|"$/, "", value)
      print value
      exit
    }
  ' "${REPO_ROOT}/Cargo.toml"
}

metadata_array_items() {
  local key="$1"
  awk -F'=' -v key="${key}" '
    /^\[workspace.metadata.pgcontext\]$/ { in_metadata = 1; next }
    /^\[/ && in_metadata { exit }
    in_metadata && $1 ~ "^[[:space:]]*" key "[[:space:]]*$" {
      value = $2
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
      gsub(/^\[|\]$/, "", value)
      gsub(/"/, "", value)
      gsub(/[[:space:]]+/, "", value)
      print value
      exit
    }
  ' "${REPO_ROOT}/Cargo.toml" | tr ',' '\n' | sed '/^$/d'
}

workspace_dependency_version() {
  local key="$1"
  awk -v key="${key}" '
    $0 ~ "^[[:space:]]*" key "[[:space:]]*=" {
      value = $0
      sub(/^[^=]*=/, "", value)
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
      if (value ~ /^\{/) {
        n = split(value, parts, ",")
        for (i = 1; i <= n; i++) {
          part = parts[i]
          gsub(/^[[:space:]\{]+|[[:space:]\}]+$/, "", part)
          if (part ~ /^version[[:space:]]*=/) {
            sub(/^version[[:space:]]*=/, "", part)
            value = part
            break
          }
        }
      }
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
      gsub(/^"|"$/, "", value)
      sub(/^=/, "", value)
      print value
      exit
    }
  ' "${REPO_ROOT}/Cargo.toml"
}

control_value() {
  local key="$1"
  awk -F'=' -v key="${key}" '
    $1 ~ "^[[:space:]]*" key "[[:space:]]*$" {
      value = $2
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
      gsub(/^'\''|'\''$/, "", value)
      print value
      exit
    }
  ' "${REPO_ROOT}/crates/context-pg/pgcontext.control"
}

mkdir -p "${out_dir}"
summary_tsv="${out_dir}/summary.tsv"
report_md="${out_dir}/report.md"
metadata_log="${out_dir}/metadata.log"

git_sha="unknown"
if git -C "${REPO_ROOT}" rev-parse --verify HEAD >/dev/null 2>&1; then
  git_sha="$(git -C "${REPO_ROOT}" rev-parse --verify HEAD)"
fi
host_os="$(uname -srm)"
rustc_version="$(rustc -V 2>/dev/null || printf 'unavailable')"
cargo_version="$(cargo -V 2>/dev/null || printf 'unavailable')"
cargo_pgrx_version="$(cargo pgrx --version 2>/dev/null || printf 'unavailable')"
started_all="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
expected_rust="$(metadata_value rust-toolchain)"
supported_postgres=()
while IFS= read -r postgres_major; do
  supported_postgres+=("${postgres_major}")
done < <(metadata_array_items supported-postgres-versions)
toolchain_rust="$(first_toml_value "${REPO_ROOT}/rust-toolchain.toml" channel)"
expected_pgrx="$(metadata_value pgrx-version)"
workspace_pgrx="$(workspace_dependency_version pgrx)"
context_pg_version="$(first_toml_value "${REPO_ROOT}/crates/context-pg/Cargo.toml" version)"
control_version="$(control_value default_version)"
metadata_mismatches=0
worktree_state="unknown"
if git -C "${REPO_ROOT}" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  if [[ -z "$(git -C "${REPO_ROOT}" status --short)" ]]; then
    worktree_state="clean"
  else
    worktree_state="dirty"
  fi
fi

if [[ "${dry_run}" -eq 0 && "${worktree_state}" == "dirty" && "${allow_dirty}" -eq 0 ]]; then
  echo "dirty worktree cannot produce release artifact evidence; use --allow-dirty for diagnostic runs" >&2
  exit 2
fi

{
  printf 'expected rust: %s\n' "${expected_rust:-missing}"
  printf 'toolchain rust: %s\n' "${toolchain_rust:-missing}"
  printf 'actual rustc: %s\n' "${rustc_version}"
  printf 'expected pgrx: %s\n' "${expected_pgrx:-missing}"
  printf 'workspace pgrx: %s\n' "${workspace_pgrx:-missing}"
  printf 'actual cargo pgrx: %s\n' "${cargo_pgrx_version}"
  printf 'context-pg version: %s\n' "${context_pg_version:-missing}"
  printf 'control default_version: %s\n' "${control_version:-missing}"
} >"${metadata_log}"

if [[ -z "${expected_rust}" || -z "${toolchain_rust}" || "${expected_rust}" != "${toolchain_rust}" ]]; then
  metadata_mismatches=$((metadata_mismatches + 1))
  printf 'metadata mismatch: rust toolchain metadata does not match rust-toolchain.toml\n' >>"${metadata_log}"
fi
if [[ "${rustc_version}" != "rustc ${expected_rust}"* ]]; then
  metadata_mismatches=$((metadata_mismatches + 1))
  printf 'metadata mismatch: rustc version does not match pinned toolchain\n' >>"${metadata_log}"
fi
if [[ -z "${expected_pgrx}" || -z "${workspace_pgrx}" || "${expected_pgrx}" != "${workspace_pgrx}" ]]; then
  metadata_mismatches=$((metadata_mismatches + 1))
  printf 'metadata mismatch: pgrx metadata does not match workspace dependency\n' >>"${metadata_log}"
fi
if ! cargo_pgrx_version_matches "${cargo_pgrx_version}" "${expected_pgrx}"; then
  metadata_mismatches=$((metadata_mismatches + 1))
  printf 'metadata mismatch: cargo pgrx version does not match pinned pgrx version\n' >>"${metadata_log}"
fi
if [[ -z "${context_pg_version}" || -z "${control_version}" || "${context_pg_version}" != "${control_version}" ]]; then
  metadata_mismatches=$((metadata_mismatches + 1))
  printf 'metadata mismatch: context-pg Cargo version does not match pgcontext.control default_version\n' >>"${metadata_log}"
fi

{
  printf 'artifact\tstatus\tsize_bytes\tsha256\tsignature_status\tsignature\tsignature_size_bytes\tsignature_sha256\tlog\n'
} >"${summary_tsv}"

{
  printf '# Release Artifact Report\n\n'
  printf -- '- Commit: `%s`\n' "${git_sha}"
  printf -- '- Worktree: `%s`\n' "${worktree_state}"
  printf -- '- Host: `%s`\n' "${host_os}"
  printf -- '- Rust: `%s`\n' "${rustc_version}"
  printf -- '- Cargo: `%s`\n' "${cargo_version}"
  printf -- '- Cargo pgrx: `%s`\n' "${cargo_pgrx_version}"
  printf -- '- Expected Rust: `%s`\n' "${expected_rust:-missing}"
  printf -- '- Expected pgrx: `%s`\n' "${expected_pgrx:-missing}"
  if [[ "${#supported_postgres[@]}" -gt 0 ]]; then
    printf -- '- Supported PostgreSQL: `%s`\n' "$(IFS=,; printf '%s' "${supported_postgres[*]}")"
  else
    printf -- '- Supported PostgreSQL: `missing`\n'
  fi
  printf -- '- context-pg version: `%s`\n' "${context_pg_version:-missing}"
  printf -- '- Control version: `%s`\n' "${control_version:-missing}"
  printf -- '- Metadata mismatches: `%s`\n' "${metadata_mismatches}"
  printf -- '- Metadata log: `%s`\n' "$(repo_relative_path "${metadata_log}")"
  printf -- '- Started: `%s`\n' "${started_all}"
  printf -- '- Require signatures: `%s`\n' "${require_signatures}"
  printf -- '- Dirty override: `%s`\n' "${allow_dirty}"
  if [[ "${dry_run}" -eq 1 ]]; then
    printf -- '- Execution: `dry-run`\n'
  else
    printf -- '- Execution: `run`\n'
  fi
  printf '\n| Artifact | Status | Size | SHA-256 | Signature | Log |\n'
  printf '|---|---|---:|---|---|---|\n'
} >"${report_md}"

total_rows=0
passed_rows=0
dry_run_rows=0
missing_rows=0
unsigned_rows=0
failed_rows=0
overall_status=0
seen_supported_sql=()
duplicate_supported_sql=0
supported_sql_artifact_symlink=0
missing_generation_logs=0
invalid_generation_logs=0

record_supported_sql_artifact() {
  local label="$1"
  local absolute="$2"
  local artifact_sha256="$3"
  local normalized_label
  local expected_label
  local major
  local seen
  local generation_log
  local generation_log_label
  local generation_log_size
  local expected_command

  normalized_label="${label}"
  if [[ "${normalized_label}" == "${REPO_ROOT}/"* ]]; then
    normalized_label="${normalized_label#"${REPO_ROOT}/"}"
  fi
  while [[ "${normalized_label}" == ./* ]]; do
    normalized_label="${normalized_label#./}"
  done
  for major in "${supported_postgres[@]}"; do
    expected_label="target/release-sql/pg${major}.sql"
    if [[ "${normalized_label}" == "${expected_label}" ]]; then
      if [[ -L "${absolute}" ]]; then
        supported_sql_artifact_symlink=1
      fi
      if generation_log="$(generation_log_for "${absolute}")"; then
        generation_log_label="$(repo_relative_path "${generation_log}")"
        generation_log_size="$(file_size "${generation_log}")"
        expected_command="cargo pgrx schema -p context-pg pg${major} --out target/release-sql/pg${major}.sql"
        if [[ -L "${generation_log}" ||
          "${generation_log_size}" -le 0 ]] ||
          ! grep -qxF "command: ${expected_command}" "${generation_log}" ||
          ! grep -qxF "commit: ${git_sha}" "${generation_log}" ||
          ! grep -qxF "artifact: ${expected_label}" "${generation_log}" ||
          ! grep -qxF "sha256: ${artifact_sha256}" "${generation_log}"; then
          invalid_generation_logs=$((invalid_generation_logs + 1))
        fi
      else
        missing_generation_logs=$((missing_generation_logs + 1))
      fi
      if [[ "${#seen_supported_sql[@]}" -gt 0 ]]; then
        for seen in "${seen_supported_sql[@]}"; do
          if [[ "${seen}" == "${major}" ]]; then
            duplicate_supported_sql=1
            return 0
          fi
        done
      fi
      seen_supported_sql+=("${major}")
      return 0
    fi
  done
}

for artifact in "${artifacts[@]}"; do
  absolute="$(artifact_path "${artifact}")"
  label="$(artifact_label "${artifact}" "${absolute}")"
  safe_name="$(printf '%s' "${label}" | tr '/ :' '---' | tr -cd '[:alnum:]._-' )"
  log_file="${out_dir}/${safe_name}.log"
  status="passed"
  size_bytes="0"
  sha256="not-run"
  signature_status="unsigned"
  signature_path=""
  signature_label=""
  signature_size_bytes="0"
  signature_sha256="not-run"
  signature_failure=""

  if [[ "${dry_run}" -eq 1 ]]; then
    status="dry-run"
    printf 'dry run artifact: %s\nabsolute path: %s\n' "${label}" "${absolute}" >"${log_file}"
  elif [[ -L "${absolute}" ]]; then
    status="failed"
    overall_status=1
    record_supported_sql_artifact "${label}" "${absolute}" "not-run"
    {
      printf 'artifact is a symlink: %s\n' "${label}"
      printf 'absolute path: %s\n' "${absolute}"
    } >"${log_file}"
  elif ! reject_external_artifact_path "${absolute}"; then
    status="failed"
    overall_status=1
    {
      printf 'artifact is outside repository: %s\n' "${label}"
      printf 'absolute path: %s\n' "${absolute}"
    } >"${log_file}"
  elif [[ ! -f "${absolute}" ]]; then
    status="missing"
    overall_status=1
    printf 'missing artifact: %s\nabsolute path: %s\n' "${label}" "${absolute}" >"${log_file}"
  else
    size_bytes="$(file_size "${absolute}")"
    if [[ "${size_bytes}" -le 0 ]]; then
      status="failed"
      overall_status=1
      printf 'artifact is empty: %s\nabsolute path: %s\n' "${label}" "${absolute}" >"${log_file}"
    else
      sha256="$(sha256_file "${absolute}")"
      if signature_path="$(signature_for "${absolute}")"; then
        signature_label="$(repo_relative_path "${signature_path}")"
        signature_size_bytes="$(file_size "${signature_path}")"
        if [[ -L "${signature_path}" ]]; then
          status="failed"
          signature_status="invalid"
          signature_failure="signature is a symlink"
          overall_status=1
        elif [[ "${signature_size_bytes}" -le 0 ]]; then
          status="failed"
          signature_status="invalid"
          signature_failure="signature is empty"
          overall_status=1
        elif ! verify_signature "${absolute}" "${signature_path}"; then
          status="failed"
          signature_status="invalid"
          signature_failure="signature verification failed"
          overall_status=1
        else
          signature_status="signed"
          signature_sha256="$(sha256_file "${signature_path}")"
        fi
      fi
      {
        printf 'artifact: %s\n' "${label}"
        printf 'absolute path: %s\n' "${absolute}"
        printf 'size bytes: %s\n' "${size_bytes}"
        printf 'sha256: %s\n' "${sha256}"
        printf 'signature status: %s\n' "${signature_status}"
        if [[ -n "${signature_path}" ]]; then
          printf 'signature: %s\n' "${signature_label}"
          printf 'signature size bytes: %s\n' "${signature_size_bytes}"
          if [[ -n "${signature_failure}" ]]; then
            printf 'signature failure: %s\n' "${signature_failure}"
          fi
          if [[ "${signature_status}" == "signed" ]]; then
            printf 'signature sha256: %s\n' "${signature_sha256}"
          fi
        fi
      } >"${log_file}"
    fi
  fi
  if [[ "${sha256}" != "not-run" ]]; then
    record_supported_sql_artifact "${label}" "${absolute}" "${sha256}"
  fi

  total_rows=$((total_rows + 1))
  case "${status}" in
    passed) passed_rows=$((passed_rows + 1)) ;;
    dry-run) dry_run_rows=$((dry_run_rows + 1)) ;;
    missing) missing_rows=$((missing_rows + 1)) ;;
    failed) failed_rows=$((failed_rows + 1)) ;;
  esac
  if [[ "${status}" == "passed" && "${signature_status}" != "signed" ]]; then
    unsigned_rows=$((unsigned_rows + 1))
  fi

  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "${label}" "${status}" "${size_bytes}" "${sha256}" "${signature_status}" \
    "${signature_label}" "${signature_size_bytes}" "${signature_sha256}" \
    "$(repo_relative_path "${log_file}")" >>"${summary_tsv}"
  printf '| `%s` | `%s` | `%s` | `%s` | `%s` | `%s` |\n' \
    "${label}" "${status}" "${size_bytes}" "${sha256}" "${signature_status}" \
    "$(repo_relative_path "${log_file}")" >>"${report_md}"
done

missing_supported_sql=()
full_release_scope=0
release_sql_dir_symlink=0
if [[ -L "${REPO_ROOT}/target" || -L "${REPO_ROOT}/target/release-sql" ]]; then
  release_sql_dir_symlink=1
fi
if [[ "${#supported_postgres[@]}" -gt 0 ]]; then
  for major in "${supported_postgres[@]}"; do
    found_major=0
    if [[ "${#seen_supported_sql[@]}" -gt 0 ]]; then
      for seen in "${seen_supported_sql[@]}"; do
        if [[ "${seen}" == "${major}" ]]; then
          found_major=1
          break
        fi
      done
    fi
    if [[ "${found_major}" -eq 0 ]]; then
      missing_supported_sql+=("pg${major}")
    fi
  done
  if [[ "${#missing_supported_sql[@]}" -eq 0 &&
        "${duplicate_supported_sql}" -eq 0 &&
        "${supported_sql_artifact_symlink}" -eq 0 &&
        "${release_sql_dir_symlink}" -eq 0 ]]; then
    full_release_scope=1
  fi
fi

{
  printf '\n## Summary\n\n'
  printf -- '- Rows: `%s`\n' "${total_rows}"
  printf -- '- Passed: `%s`\n' "${passed_rows}"
  printf -- '- Dry-run: `%s`\n' "${dry_run_rows}"
  printf -- '- Missing: `%s`\n' "${missing_rows}"
  printf -- '- Failed: `%s`\n' "${failed_rows}"
  printf -- '- Unsigned: `%s`\n' "${unsigned_rows}"
  printf -- '- Metadata mismatches: `%s`\n' "${metadata_mismatches}"
  printf -- '- Missing generation logs: `%s`\n' "${missing_generation_logs}"
  printf -- '- Invalid generation logs: `%s`\n' "${invalid_generation_logs}"
  if [[ "${#missing_supported_sql[@]}" -gt 0 ]]; then
    printf -- '- Missing supported SQL artifacts: `%s`\n' "$(IFS=,; printf '%s' "${missing_supported_sql[*]}")"
  else
    printf -- '- Missing supported SQL artifacts: `none`\n'
  fi
  printf -- '- Duplicate supported SQL artifacts: `%s`\n' "${duplicate_supported_sql}"
  printf -- '- Supported SQL artifact symlink: `%s`\n' "${supported_sql_artifact_symlink}"
  printf -- '- Release SQL directory symlink: `%s`\n' "${release_sql_dir_symlink}"
  printf -- '- Full release scope: `%s`\n' "${full_release_scope}"
  if [[ "${dry_run_rows}" -gt 0 ||
        "${missing_rows}" -gt 0 ||
        "${failed_rows}" -gt 0 ||
        "${missing_generation_logs}" -gt 0 ||
        "${invalid_generation_logs}" -gt 0 ||
        "${metadata_mismatches}" -gt 0 ||
        "${full_release_scope}" -ne 1 ||
        "${worktree_state}" != "clean" ||
        "${allow_dirty}" -ne 0 ||
        "${passed_rows}" -ne "${total_rows}" ||
        ( "${require_signatures}" -eq 1 && "${unsigned_rows}" -gt 0 ) ]]; then
    printf -- '- Approval: `incomplete`\n'
    printf -- '- Approval note: release artifact evidence requires a clean tree, matching pinned toolchain metadata, exactly one non-symlink generated SQL artifact for every supported PostgreSQL major from a non-symlink `target/release-sql` directory, non-symlink per-artifact generation logs from the expected cargo pgrx schema command, no dry-run rows, no missing or failed artifacts, and verified non-symlink signatures for every artifact when `--require-signatures` is used.\n'
  else
    printf -- '- Approval: `complete`\n'
  fi
  printf '\nSummary TSV: `%s`\n' "$(repo_relative_path "${summary_tsv}")"
} >>"${report_md}"

printf 'release artifact report: %s\n' "${report_md}"
if [[ "${require_signatures}" -eq 1 && "${unsigned_rows}" -gt 0 ]]; then
  overall_status=1
fi
if [[ "${missing_generation_logs}" -gt 0 || "${invalid_generation_logs}" -gt 0 ]]; then
  overall_status=1
fi
exit "${overall_status}"
