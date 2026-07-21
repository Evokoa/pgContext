#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RUNNER="${REPO_ROOT}/scripts/run-pg17-recovery-report.sh"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/pgcontext-recovery-report.XXXXXX")"
trap 'rm -rf "${tmp_dir}"' EXIT

plan_one="${tmp_dir}/plan-one.tsv"
plan_two="${tmp_dir}/plan-two.tsv"
"${RUNNER}" --pg-major 17 --plan >"${plan_one}"
"${RUNNER}" --pg-major 17 --plan >"${plan_two}"
cmp "${plan_one}" "${plan_two}"
head -n 1 "${plan_one}" | grep -Fqx $'gate\tkind\towner\ttools\tcallback\tfailpoints\tscript\tcommand'
[[ "$(tail -n +2 "${plan_one}" | wc -l | tr -d ' ')" == "3" ]]
grep -Fq $'hnsw-wal-crash-replay\tcrash-replay\tcontext-pg\tcargo-pgrx,pg_ctl,psql\tHnswPhysicalFailpoint\tbefore_page_initialization,after_page_initialization,before_append,after_append,before_rewiring,after_rewiring,before_generic_xlog_finish,after_generic_xlog_finish,before_metapage_publication,after_metapage_publication\ttests/heavy/hnsw_wal_crash_replay.sh' "${plan_one}"
grep -Fq $'hnsw-standby-promotion\tstandby-promotion\tcontext-pg\tpg_basebackup,pg_ctl,psql\tnone\tnone\ttests/heavy/hnsw_replica_promotion.sh' "${plan_one}"
grep -Fq $'hnsw-relation-kinds\trelation-kinds\tcontext-pg\tcargo-pgrx,psql\tnone\tnone\ttests/heavy/hnsw_relation_kinds.sh' "${plan_one}"

if "${RUNNER}" --pg-major 17 >"${tmp_dir}/missing-mode.out" 2>&1; then
  echo "recovery runner accepted missing mode" >&2
  exit 1
fi
grep -Fq 'choose one of --plan, --dry-run, or --approve' "${tmp_dir}/missing-mode.out"

ln -s / "${tmp_dir}/root-link"
for root_alias in / /./ // "${tmp_dir}/root-link" "${tmp_dir}/root-link/."; do
  if "${RUNNER}" --pg-major 17 --dry-run --out-dir "${root_alias}" >"${tmp_dir}/root.out" 2>&1; then
    echo "recovery runner accepted root output alias" >&2
    exit 1
  fi
  grep -Fq -- '--out-dir must be a non-root path' "${tmp_dir}/root.out"
done

dry_dir="${tmp_dir}/dry"
"${RUNNER}" --pg-major 17 --dry-run --out-dir "${dry_dir}"
[[ "$(awk -F '\t' 'NR > 1 && $4 == "dry-run" { count++ } END { print count + 0 }' "${dry_dir}/summary.tsv")" == "3" ]]
grep -Fq -- '- Execution: `dry-run`' "${dry_dir}/report.md"
grep -Fq -- '- Approval: `incomplete`' "${dry_dir}/report.md"

fake_bin="${tmp_dir}/bin"
mkdir -p "${fake_bin}"
fake_log="${tmp_dir}/fake.log"
for script in hnsw_wal_crash_replay hnsw_replica_promotion hnsw_relation_kinds; do
cat >"${fake_bin}/${script}.sh" <<'FAKE_SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
printf '%s|%s\n' "$(basename "$0")" "$*" >>"${FAKE_RECOVERY_LOG}"
if [[ "${FAKE_FAIL_STANDBY:-0}" == "1" && "$(basename "$0")" == "hnsw_replica_promotion.sh" ]]; then
  exit 41
fi
FAKE_SCRIPT
  chmod +x "${fake_bin}/${script}.sh"
done
cat >"${fake_bin}/git" <<'FAKE_GIT'
#!/usr/bin/env bash
set -euo pipefail
FAKE_GIT
chmod +x "${fake_bin}/git"

approve_dir="${tmp_dir}/approve"
FAKE_RECOVERY_LOG="${fake_log}" \
GIT_BIN="${fake_bin}/git" \
WAL_REPLAY_SCRIPT="${fake_bin}/hnsw_wal_crash_replay.sh" \
STANDBY_SCRIPT="${fake_bin}/hnsw_replica_promotion.sh" \
RELATIONS_SCRIPT="${fake_bin}/hnsw_relation_kinds.sh" \
  "${RUNNER}" --pg-major 17 --approve --pgrx-data-dir /tmp/fake-pgrx --out-dir "${approve_dir}"
[[ "$(wc -l <"${fake_log}" | tr -d ' ')" == "3" ]]
grep -Fq -- '- Approval: `complete`' "${approve_dir}/report.md"

fail_dir="${tmp_dir}/fail"
if FAKE_RECOVERY_LOG="${tmp_dir}/fail.log" \
  FAKE_FAIL_STANDBY=1 \
  GIT_BIN="${fake_bin}/git" \
  WAL_REPLAY_SCRIPT="${fake_bin}/hnsw_wal_crash_replay.sh" \
  STANDBY_SCRIPT="${fake_bin}/hnsw_replica_promotion.sh" \
  RELATIONS_SCRIPT="${fake_bin}/hnsw_relation_kinds.sh" \
  "${RUNNER}" --pg-major 17 --approve --pgrx-data-dir /tmp/fake-pgrx --out-dir "${fail_dir}" \
    >"${tmp_dir}/fail.out" 2>&1
then
  echo "recovery runner accepted a failing standby row" >&2
  exit 1
fi
grep -Fq $'hnsw-standby-promotion\tstandby-promotion\tcontext-pg\tfail\t41' "${fail_dir}/summary.tsv"
grep -Fq -- '- Approval: `incomplete`' "${fail_dir}/report.md"
grep -Fq 'PG17 recovery report contains failing rows' "${tmp_dir}/fail.out"

if "${RUNNER}" --pg-major 17 --approve --out-dir "${tmp_dir}/no-dir" >"${tmp_dir}/no-dir.out" 2>&1; then
  echo "recovery runner approved without a pgrx data directory" >&2
  exit 1
fi
grep -Fq -- '--pgrx-data-dir is required with --approve' "${tmp_dir}/no-dir.out"

echo "PG17 recovery report smoke tests passed"
