#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
TMPDIR="${TMPDIR:-${REPO_ROOT}/target/tmp}"
mkdir -p "${TMPDIR}"
work_dir="$(mktemp -d "${TMPDIR}/crate-boundaries-test.XXXXXX")"
trap 'rm -rf "${work_dir}"' EXIT

make_fixture() {
  local root="$1"
  mkdir -p "${root}/scripts" "${root}/stubs"
  cp "${REPO_ROOT}/scripts/check-crate-boundaries.sh" "${root}/scripts/"
  chmod +x "${root}/scripts/check-crate-boundaries.sh"
  cat >"${root}/Cargo.toml" <<'EOF'
[workspace]
members = ["crates/*"]
resolver = "2"
EOF
  for dependency in serde serde_json proptest pgrx postgres; do
    mkdir -p "${root}/stubs/${dependency}/src"
    cat >"${root}/stubs/${dependency}/Cargo.toml" <<EOF
[package]
name = "${dependency}"
version = "0.1.0"
edition = "2024"
EOF
    printf '%s\n' 'pub fn fixture() {}' >"${root}/stubs/${dependency}/src/lib.rs"
  done
  for crate in core filter hybrid index storage query build pg; do
    mkdir -p "${root}/crates/context-${crate}/src"
    printf '%s\n' 'pub fn fixture() {}' >"${root}/crates/context-${crate}/src/lib.rs"
    cat >"${root}/crates/context-${crate}/Cargo.toml" <<EOF
[package]
name = "context-${crate}"
version = "0.1.0"

[dependencies]
EOF
  done
  cat >>"${root}/crates/context-query/Cargo.toml" <<'EOF'
context-core = { path = "../context-core" }
context-filter = { path = "../context-filter" }
context-hybrid = { path = "../context-hybrid" }
serde = { path = "../../stubs/serde" }
serde_json = { path = "../../stubs/serde_json" }

[dev-dependencies]
proptest = { path = "../../stubs/proptest" }
EOF
  cat >>"${root}/crates/context-build/Cargo.toml" <<'EOF'
context-core = { path = "../context-core" }
EOF
  cat >>"${root}/crates/context-index/Cargo.toml" <<'EOF'
context-core = { path = "../context-core" }
EOF
  cat >>"${root}/crates/context-storage/Cargo.toml" <<'EOF'
context-core = { path = "../context-core" }
EOF
  cat >>"${root}/crates/context-pg/Cargo.toml" <<'EOF'
context-core = { path = "../context-core" }
context-query = { path = "../context-query" }
context-build = { path = "../context-build" }
pgrx = { path = "../../stubs/pgrx" }
EOF
  cat >"${root}/crates/context-pg/src/lib.rs" <<'EOF'
use pgrx::prelude::*;
const SQLSTATE: &str = "22023";
EOF
  mkdir -p "${root}/crates/context-build/tests"
  cat >"${root}/crates/context-build/tests/contract.rs" <<'EOF'
use context_build::fixture;
EOF
}

expect_failure() {
  local root="$1"
  local message="$2"
  local label="$3"
  if (cd "${root}" && scripts/check-crate-boundaries.sh) >"${root}/failure.err" 2>&1; then
    echo "${label} should fail" >&2
    exit 1
  fi
  grep -Fq "${message}" "${root}/failure.err"
}

good_root="${work_dir}/good"
make_fixture "${good_root}"
(cd "${good_root}" && scripts/check-crate-boundaries.sh)

query_dep_root="${work_dir}/query-dependency"
make_fixture "${query_dep_root}"
perl -0pi -e 's#serde = \{ path = "../../stubs/serde" \}#context-index = { path = "../context-index" }\nserde = { path = "../../stubs/serde" }#' \
  "${query_dep_root}/crates/context-query/Cargo.toml"
expect_failure "${query_dep_root}" \
  'context-query dependency is not allowed: context-index' \
  'forbidden context-query dependency'

target_dep_root="${work_dir}/target-dependency"
make_fixture "${target_dep_root}"
cat >>"${target_dep_root}/crates/context-query/Cargo.toml" <<'EOF'

[target.'cfg(unix)'.dependencies]
context-storage = { path = "../context-storage" }
EOF
expect_failure "${target_dep_root}" \
  'context-query dependency is not allowed: context-storage' \
  'forbidden target-specific context-query dependency'

dotted_dep_root="${work_dir}/dotted-dependency"
make_fixture "${dotted_dep_root}"
cat >>"${dotted_dep_root}/crates/context-query/Cargo.toml" <<'EOF'

[dependencies.context-index]
path = "../context-index"
EOF
expect_failure "${dotted_dep_root}" \
  'context-query dependency is not allowed: context-index' \
  'forbidden dotted-table context-query dependency'

missing_query_dep_root="${work_dir}/missing-query-dependency"
make_fixture "${missing_query_dep_root}"
perl -0pi -e 's/^context-hybrid[^\n]*\n//m' \
  "${missing_query_dep_root}/crates/context-query/Cargo.toml"
expect_failure "${missing_query_dep_root}" \
  'context-query required dependency is missing: context-hybrid' \
  'missing required context-query dependency'

build_dep_root="${work_dir}/build-dependency"
make_fixture "${build_dep_root}"
printf '%s\n' 'serde = { path = "../../stubs/serde" }' \
  >>"${build_dep_root}/crates/context-build/Cargo.toml"
expect_failure "${build_dep_root}" \
  'context-build dependency is not allowed: serde' \
  'forbidden context-build dependency'

missing_build_dep_root="${work_dir}/missing-build-dependency"
make_fixture "${missing_build_dep_root}"
perl -0pi -e 's/^context-core[^\n]*\n//m' \
  "${missing_build_dep_root}/crates/context-build/Cargo.toml"
expect_failure "${missing_build_dep_root}" \
  'context-build required dependency is missing: context-core' \
  'missing required context-build dependency'

missing_pg_query_root="${work_dir}/missing-pg-query-dependency"
make_fixture "${missing_pg_query_root}"
perl -0pi -e 's/^context-query[^\n]*\n//m' \
  "${missing_pg_query_root}/crates/context-pg/Cargo.toml"
expect_failure "${missing_pg_query_root}" \
  'context-pg required dependency is missing: context-query' \
  'missing context-pg query composition dependency'

missing_pg_build_root="${work_dir}/missing-pg-build-dependency"
make_fixture "${missing_pg_build_root}"
perl -0pi -e 's/^context-build[^\n]*\n//m' \
  "${missing_pg_build_root}/crates/context-pg/Cargo.toml"
expect_failure "${missing_pg_build_root}" \
  'context-pg required dependency is missing: context-build' \
  'missing context-pg build composition dependency'

pure_pg_root="${work_dir}/pure-postgres-import"
make_fixture "${pure_pg_root}"
printf '%s\n' 'use pgrx::prelude::*;' >>"${pure_pg_root}/crates/context-core/src/lib.rs"
expect_failure "${pure_pg_root}" \
  'PostgreSQL import leaked into pure crate source' \
  'pure PostgreSQL import'

sqlstate_root="${work_dir}/pure-sqlstate"
make_fixture "${sqlstate_root}"
printf '%s\n' 'const CODE: &str = "23505";' >>"${sqlstate_root}/crates/context-query/src/lib.rs"
expect_failure "${sqlstate_root}" \
  'SQLSTATE transport policy leaked into pure crate source' \
  'pure SQLSTATE literal'

build_script_root="${work_dir}/build-script-postgres-import"
make_fixture "${build_script_root}"
printf '%s\n' 'use pgrx as pg;' >"${build_script_root}/crates/context-hybrid/build.rs"
expect_failure "${build_script_root}" \
  'PostgreSQL import leaked into pure crate source' \
  'build script PostgreSQL import'

reverse_dep_root="${work_dir}/reverse-import"
make_fixture "${reverse_dep_root}"
printf '%s\n' 'use context_query::QueryIr;' >>"${reverse_dep_root}/crates/context-index/src/lib.rs"
expect_failure "${reverse_dep_root}" \
  'context-index source imports a forbidden sibling crate' \
  'reverse query import'

index_storage_root="${work_dir}/index-storage-dependency"
make_fixture "${index_storage_root}"
printf '%s\n' 'context-storage = { path = "../context-storage" }' \
  >>"${index_storage_root}/crates/context-index/Cargo.toml"
expect_failure "${index_storage_root}" \
  'context-index dependency is forbidden: context-storage' \
  'index to storage dependency'

storage_index_root="${work_dir}/storage-index-dependency"
make_fixture "${storage_index_root}"
printf '%s\n' 'context-index = { path = "../context-index" }' \
  >>"${storage_index_root}/crates/context-storage/Cargo.toml"
expect_failure "${storage_index_root}" \
  'context-storage dependency is forbidden: context-index' \
  'storage to index dependency'

query_import_root="${work_dir}/query-infrastructure-import"
make_fixture "${query_import_root}"
printf '%s\n' 'use context_storage::Segment;' \
  >>"${query_import_root}/crates/context-query/src/lib.rs"
expect_failure "${query_import_root}" \
  'context-query source imports an infrastructure crate' \
  'query infrastructure import'

build_import_root="${work_dir}/build-forbidden-import"
make_fixture "${build_import_root}"
printf '%s\n' 'use context_filter::Filter;' \
  >>"${build_import_root}/crates/context-build/src/lib.rs"
expect_failure "${build_import_root}" \
  'context-build source imports a crate other than context-core' \
  'build forbidden source import'

query_filesystem_root="${work_dir}/query-filesystem"
make_fixture "${query_filesystem_root}"
printf '%s\n' 'use std::fs::File;' \
  >>"${query_filesystem_root}/crates/context-query/src/lib.rs"
expect_failure "${query_filesystem_root}" \
  'filesystem API leaked into context-query' \
  'query filesystem import'

nested_filesystem_root="${work_dir}/nested-filesystem"
make_fixture "${nested_filesystem_root}"
printf '%s\n' 'use std::{fs as filesystem, path::PathBuf};' \
  >>"${nested_filesystem_root}/crates/context-index/src/lib.rs"
expect_failure "${nested_filesystem_root}" \
  'filesystem API leaked into context-index' \
  'nested aliased filesystem import'

direct_alias_root="${work_dir}/direct-filesystem-alias"
make_fixture "${direct_alias_root}"
printf '%s\n' 'use std::fs as filesystem;' \
  >>"${direct_alias_root}/crates/context-query/src/lib.rs"
expect_failure "${direct_alias_root}" \
  'filesystem API leaked into context-query' \
  'direct filesystem alias'

pure_manifest_root="${work_dir}/pure-postgres-manifest"
make_fixture "${pure_manifest_root}"
printf '%s\n' 'pgrx = { path = "../../stubs/pgrx" }' \
  >>"${pure_manifest_root}/crates/context-filter/Cargo.toml"
expect_failure "${pure_manifest_root}" \
  'PostgreSQL dependency leaked into pure crate manifest: context-filter' \
  'pure PostgreSQL manifest dependency'

renamed_pg_root="${work_dir}/renamed-postgres-manifest"
make_fixture "${renamed_pg_root}"
printf '%s\n' 'pg = { package = "pgrx", path = "../../stubs/pgrx" }' \
  >>"${renamed_pg_root}/crates/context-filter/Cargo.toml"
expect_failure "${renamed_pg_root}" \
  'PostgreSQL dependency leaked into pure crate manifest: context-filter' \
  'renamed PostgreSQL manifest dependency'

postgres_import_root="${work_dir}/postgres-import"
make_fixture "${postgres_import_root}"
printf '%s\n' 'use postgres as database;' \
  >>"${postgres_import_root}/crates/context-core/src/lib.rs"
expect_failure "${postgres_import_root}" \
  'PostgreSQL import leaked into pure crate source' \
  'postgres alias source import'

benign_root="${work_dir}/benign-near-match"
make_fixture "${benign_root}"
cat >>"${benign_root}/crates/context-query/src/lib.rs" <<'EOF'
//! SQLSTATE translation remains adapter-owned.
const NOT_A_CODE: &str = "ABCDE";
const TOO_SHORT: &str = "2202";
EOF
(cd "${benign_root}" && scripts/check-crate-boundaries.sh)
