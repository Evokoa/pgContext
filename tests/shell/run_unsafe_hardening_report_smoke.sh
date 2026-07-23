#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
RUNNER="${REPO_ROOT}/scripts/run-unsafe-hardening-report.sh"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/pgcontext-unsafe-hardening.XXXXXX")"
trap 'rm -rf "${tmp_dir}"' EXIT

plan_one="${tmp_dir}/plan-one.tsv"
plan_two="${tmp_dir}/plan-two.tsv"
"${RUNNER}" --pg-major 17 --plan >"${plan_one}"
"${RUNNER}" --pg-major 17 --plan >"${plan_two}"
cmp "${plan_one}" "${plan_two}"

head -n 1 "${plan_one}" | grep -Fqx $'gate\tkind\towner\tpg_major\tcommand'
[[ "$(tail -n +2 "${plan_one}" | wc -l | tr -d ' ')" == "8" ]]
grep -Fqx $'callback-source-inventory\tstatic\tcontext-pg\t17\tscripts/check-hnsw-callback-guards.sh' "${plan_one}"
grep -Fqx $'unsafe-safety-comments\tstatic\tworkspace\t17\tscripts/check-unsafe-safety-comments.sh' "${plan_one}"
grep -Fqx $'storage-segment-miri\tmiri\tcontext-storage\t17\tMIRIFLAGS=-Zmiri-disable-isolation cargo +nightly miri test -p context-storage --test segment_format' "${plan_one}"
grep -Fqx $'storage-mapped-view-miri\tmiri\tcontext-storage\t17\tMIRIFLAGS=-Zmiri-disable-isolation cargo +nightly miri test -p context-storage --test mapped_hnsw_view' "${plan_one}"
grep -Fqx $'storage-mapped-real-asan\tasan\tcontext-storage\t17\tRUSTFLAGS=-Zsanitizer=address cargo +nightly test -Zbuild-std --target <host-target> -p context-storage --test mapped_generation_subprocess' "${plan_one}"
grep -Fqx $'storage-mapped-real-tsan\ttsan\tcontext-storage\t17\tRUSTFLAGS=-Zsanitizer=thread cargo +nightly test -Zbuild-std --target <host-target> -p context-storage --test mapped_generation_subprocess' "${plan_one}"
grep -Fqx $'hnsw-pg17-asan\tasan\tcontext-pg\t17\tPG_CONFIG=<pg17-config> RUSTFLAGS=-Zsanitizer=address cargo +nightly pgrx test -p context-pg pg17 hnsw' "${plan_one}"
grep -Fqx $'hnsw-pg17-tsan\ttsan\tcontext-pg\t17\tPG_CONFIG=<pg17-config> RUSTFLAGS=-Zsanitizer=thread cargo +nightly pgrx test -p context-pg pg17 hnsw' "${plan_one}"

if "${RUNNER}" --pg-major 16 --plan >"${tmp_dir}/bad-major.out" 2>&1; then
  echo "unsafe hardening runner accepted unsupported PG16" >&2
  exit 1
fi
grep -Fq -- '--pg-major must be 17' "${tmp_dir}/bad-major.out"

ln -s / "${tmp_dir}/root-link"
for root_alias in / /./ // "${tmp_dir}/root-link" "${tmp_dir}/root-link/."; do
  if "${RUNNER}" --pg-major 17 --dry-run --out-dir "${root_alias}" \
    >"${tmp_dir}/root-alias.out" 2>&1
  then
    echo "unsafe hardening runner accepted root output alias: ${root_alias}" >&2
    exit 1
  fi
  grep -Fq -- '--out-dir must be a non-root path' "${tmp_dir}/root-alias.out"
done

fake_bin="${tmp_dir}/bin"
mkdir -p "${fake_bin}"
fake_log="${tmp_dir}/fake.log"

cat >"${fake_bin}/cargo" <<'FAKE_CARGO'
#!/usr/bin/env bash
set -euo pipefail
printf 'cargo|RUSTFLAGS=%s|MIRIFLAGS=%s|PG_CONFIG=%s|%s\n' \
  "${RUSTFLAGS:-}" "${MIRIFLAGS:-}" "${PG_CONFIG:-}" "$*" >>"${FAKE_HARDENING_LOG}"
if [[ "${FAKE_FAIL_TSAN:-0}" == "1" && "${RUSTFLAGS:-}" == "-Zsanitizer=thread" ]]; then
  exit 41
fi
FAKE_CARGO
cat >"${fake_bin}/pg_config" <<'FAKE_PG_CONFIG'
#!/usr/bin/env bash
set -euo pipefail
printf 'PostgreSQL 17.99-test\n'
FAKE_PG_CONFIG
cat >"${fake_bin}/callback-checker" <<'FAKE_CHECKER'
#!/usr/bin/env bash
set -euo pipefail
printf 'callback-checker|%s\n' "$*" >>"${FAKE_HARDENING_LOG}"
FAKE_CHECKER
cat >"${fake_bin}/unsafe-checker" <<'FAKE_CHECKER'
#!/usr/bin/env bash
set -euo pipefail
printf 'unsafe-checker|%s\n' "$*" >>"${FAKE_HARDENING_LOG}"
FAKE_CHECKER
chmod +x "${fake_bin}"/*

dry_dir="${tmp_dir}/dry"
FAKE_HARDENING_LOG="${fake_log}" \
CARGO_BIN="${fake_bin}/cargo" \
PG_CONFIG="${fake_bin}/pg_config" \
HNSW_CALLBACK_CHECKER="${fake_bin}/callback-checker" \
UNSAFE_COMMENT_CHECKER="${fake_bin}/unsafe-checker" \
  "${RUNNER}" --pg-major 17 --dry-run --out-dir "${dry_dir}"

[[ ! -e "${fake_log}" ]]
[[ "$(awk -F '\t' 'NR > 1 && $4 == "dry-run" { count++ } END { print count + 0 }' "${dry_dir}/summary.tsv")" == "8" ]]
grep -Fq -- '- Execution: `dry-run`' "${dry_dir}/report.md"

run_dir="${tmp_dir}/run"
FAKE_HARDENING_LOG="${fake_log}" \
CARGO_BIN="${fake_bin}/cargo" \
PG_CONFIG="${fake_bin}/pg_config" \
HNSW_CALLBACK_CHECKER="${fake_bin}/callback-checker" \
UNSAFE_COMMENT_CHECKER="${fake_bin}/unsafe-checker" \
  "${RUNNER}" --pg-major 17 --out-dir "${run_dir}"

[[ "$(wc -l <"${fake_log}" | tr -d ' ')" == "8" ]]
grep -Fqx 'callback-checker|' "${fake_log}"
grep -Fqx 'unsafe-checker|' "${fake_log}"
grep -Fq 'cargo|RUSTFLAGS=|MIRIFLAGS=-Zmiri-disable-isolation|PG_CONFIG=|+nightly miri test -p context-storage --test segment_format' "${fake_log}"
grep -Fq 'cargo|RUSTFLAGS=|MIRIFLAGS=-Zmiri-disable-isolation|PG_CONFIG=|+nightly miri test -p context-storage --test mapped_hnsw_view' "${fake_log}"
grep -Fq 'cargo|RUSTFLAGS=-Zsanitizer=address|MIRIFLAGS=|PG_CONFIG=|+nightly test -Zbuild-std --target ' "${fake_log}"
grep -Fq 'cargo|RUSTFLAGS=-Zsanitizer=thread|MIRIFLAGS=|PG_CONFIG=|+nightly test -Zbuild-std --target ' "${fake_log}"
grep -Fq 'cargo|RUSTFLAGS=-Zsanitizer=address|MIRIFLAGS=|PG_CONFIG='"${fake_bin}/pg_config"'|+nightly pgrx test -p context-pg pg17 hnsw' "${fake_log}"
grep -Fq 'cargo|RUSTFLAGS=-Zsanitizer=thread|MIRIFLAGS=|PG_CONFIG='"${fake_bin}/pg_config"'|+nightly pgrx test -p context-pg pg17 hnsw' "${fake_log}"
[[ "$(awk -F '\t' 'NR > 1 && $4 == "pass" && $5 == "0" { count++ } END { print count + 0 }' "${run_dir}/summary.tsv")" == "8" ]]
grep -Fq -- '- Execution: `run`' "${run_dir}/report.md"

fail_dir="${tmp_dir}/fail"
fail_log="${tmp_dir}/fail.log"
if FAKE_HARDENING_LOG="${fail_log}" \
  FAKE_FAIL_TSAN=1 \
  CARGO_BIN="${fake_bin}/cargo" \
  PG_CONFIG="${fake_bin}/pg_config" \
  HNSW_CALLBACK_CHECKER="${fake_bin}/callback-checker" \
  UNSAFE_COMMENT_CHECKER="${fake_bin}/unsafe-checker" \
    "${RUNNER}" --pg-major 17 --out-dir "${fail_dir}" \
    >"${tmp_dir}/fail.out" 2>&1
then
  echo "unsafe hardening runner accepted a failing TSan row" >&2
  exit 1
fi
grep -Fq $'hnsw-pg17-tsan\ttsan\tcontext-pg\tfail\t41' "${fail_dir}/summary.tsv"
[[ "$(tail -n +2 "${fail_dir}/summary.tsv" | wc -l | tr -d ' ')" == "8" ]]
[[ "$(wc -l <"${fail_log}" | tr -d ' ')" == "8" ]]
grep -Fq 'unsafe hardening report contains failing rows' "${tmp_dir}/fail.out"

missing_root_dir="${tmp_dir}/missing-root-report"
missing_root_log="${tmp_dir}/missing-root.log"
if REPO_ROOT="${tmp_dir}/does-not-exist" \
  FAKE_HARDENING_LOG="${missing_root_log}" \
  CARGO_BIN="${fake_bin}/cargo" \
  PG_CONFIG="${fake_bin}/pg_config" \
  HNSW_CALLBACK_CHECKER="${fake_bin}/callback-checker" \
  UNSAFE_COMMENT_CHECKER="${fake_bin}/unsafe-checker" \
    "${RUNNER}" --pg-major 17 --out-dir "${missing_root_dir}" \
    >"${tmp_dir}/missing-root.out" 2>&1
then
  echo "unsafe hardening runner accepted a missing repository root" >&2
  exit 1
fi
[[ ! -e "${missing_root_log}" ]]
[[ "$(awk -F '\t' 'NR > 1 && $4 == "fail" { count++ } END { print count + 0 }' "${missing_root_dir}/summary.tsv")" == "8" ]]
grep -Fq 'unsafe hardening report contains failing rows' "${tmp_dir}/missing-root.out"

echo "unsafe hardening report smoke tests passed"
