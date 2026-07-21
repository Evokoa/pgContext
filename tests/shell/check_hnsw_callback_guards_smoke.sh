#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
CHECKER="${REPO_ROOT}/scripts/check-hnsw-callback-guards.sh"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/pgcontext-hnsw-callback-smoke.XXXXXX")"
trap 'rm -rf "${tmp_dir}"' EXIT

make_fixture() {
  local name="$1"
  local root="${tmp_dir}/${name}"
  mkdir -p "${root}"
  cp "${REPO_ROOT}/crates/context-pg/src/hnsw_am.rs" "${root}/hnsw_am.rs"
  cp "${REPO_ROOT}/crates/context-pg/src/hnsw_am_callbacks.rs" \
    "${root}/hnsw_am_callbacks.rs"
  cp "${REPO_ROOT}/crates/context-pg/src/hnsw_am_page_storage.rs" \
    "${root}/hnsw_am_page_storage.rs"
  cp "${REPO_ROOT}/crates/context-pg/src/hnsw_am_validation.rs" \
    "${root}/hnsw_am_validation.rs"
  cp -R "${REPO_ROOT}/crates/context-pg/src/hnsw_am" "${root}/hnsw_am"
  printf '%s\n' "${root}"
}

replace_once() {
  local file="$1"
  local before="$2"
  local after="$3"
  BEFORE="${before}" AFTER="${after}" perl -0pi -e '
    BEGIN { $before = $ENV{"BEFORE"}; $after = $ENV{"AFTER"}; }
    $count = s/\Q$before\E/$after/;
    END { die "expected exactly one replacement\n" unless $count == 1; }
  ' "${file}"
}

run_checker() {
  local root="$1"
  HNSW_AM_SOURCE="${root}/hnsw_am.rs" \
  HNSW_OPTIONS_SOURCE="${root}/hnsw_am/options.rs" \
  HNSW_CONTRACT_SOURCE="${root}/hnsw_am/callback_contract.rs" \
  HNSW_MODULE_ROOT="${root}/hnsw_am" \
  HNSW_PAGE_STORAGE_SOURCE="${root}/hnsw_am_page_storage.rs" \
  HNSW_UNSAFE_INVENTORY="${root}/hnsw_am/unsafe_inventory.data" \
    "${CHECKER}"
}

expect_failure() {
  local name="$1"
  local expected="$2"
  local root="$3"
  local output="${tmp_dir}/${name}.out"
  if run_checker "${root}" >"${output}" 2>&1; then
    echo "checker unexpectedly accepted ${name}" >&2
    exit 1
  fi
  if ! grep -Fq "${expected}" "${output}"; then
    echo "checker failure for ${name} did not contain: ${expected}" >&2
    cat "${output}" >&2
    exit 1
  fi
}

baseline="$(make_fixture baseline)"
run_checker "${baseline}" >/dev/null

missing_guard="$(make_fixture missing-guard)"
replace_once "${missing_guard}/hnsw_am_callbacks.rs" \
  $'#[pg_guard]\n#[allow(unused_qualifications)]\n// SAFETY: The opclass OID is a copied scalar supplied by PostgreSQL.\nunsafe extern "C-unwind" fn pgcontext_hnsw_validate' \
  $'// #[pg_guard]\n#[allow(unused_qualifications)]\n// SAFETY: The opclass OID is a copied scalar supplied by PostgreSQL.\nunsafe extern "C-unwind" fn pgcontext_hnsw_validate'
expect_failure missing-guard 'missing #[pg_guard]: pgcontext_hnsw_validate' "${missing_guard}"

block_commented_guard="$(make_fixture block-commented-guard)"
replace_once "${block_commented_guard}/hnsw_am_callbacks.rs" \
  $'#[pg_guard]\n#[allow(unused_qualifications)]\n// SAFETY: The opclass OID is a copied scalar supplied by PostgreSQL.\nunsafe extern "C-unwind" fn pgcontext_hnsw_validate' \
  $'/*\n#[pg_guard]\n*/\n#[allow(unused_qualifications)]\n// SAFETY: The opclass OID is a copied scalar supplied by PostgreSQL.\nunsafe extern "C-unwind" fn pgcontext_hnsw_validate'
rustfmt --edition 2024 --emit stdout "${block_commented_guard}/hnsw_am_callbacks.rs" >/dev/null
expect_failure block-commented-guard 'missing #[pg_guard]: pgcontext_hnsw_validate' \
  "${block_commented_guard}"

missing_safety="$(make_fixture missing-safety)"
replace_once "${missing_safety}/hnsw_am_callbacks.rs" \
  '// SAFETY: The opclass OID is a copied scalar supplied by PostgreSQL.' \
  '// unrelated mention of // SAFETY: is not an attached contract.'
expect_failure missing-safety 'missing a local SAFETY contract: pgcontext_hnsw_validate' "${missing_safety}"

block_commented_safety="$(make_fixture block-commented-safety)"
replace_once "${block_commented_safety}/hnsw_am_callbacks.rs" \
  '// SAFETY: The opclass OID is a copied scalar supplied by PostgreSQL.' \
  $'/*\n// SAFETY: block-comment decoy.\n*/'
rustfmt --edition 2024 --emit stdout "${block_commented_safety}/hnsw_am_callbacks.rs" >/dev/null
expect_failure block-commented-safety \
  'missing a local SAFETY contract: pgcontext_hnsw_validate' \
  "${block_commented_safety}"

missing_delegation="$(make_fixture missing-delegation)"
replace_once "${missing_delegation}/hnsw_am_callbacks.rs" \
  '    self::hnsw_validate_safe(opclass_oid)' \
  $'    // hnsw_validate_safe(opclass_oid) is not a delegation.\n    let _ = opclass_oid; true'
expect_failure missing-delegation 'does not delegate to its safe function' "${missing_delegation}"

string_delegation="$(make_fixture string-delegation)"
replace_once "${string_delegation}/hnsw_am_callbacks.rs" \
  '    self::hnsw_validate_safe(opclass_oid)' \
  '    let _name = "hnsw_validate_safe("; let _ = (opclass_oid, _name); true'
expect_failure string-delegation 'does not delegate to its safe function' "${string_delegation}"

dead_delegation="$(make_fixture dead-delegation)"
replace_once "${dead_delegation}/hnsw_am_callbacks.rs" \
  '    self::hnsw_validate_safe(opclass_oid)' \
  '    if false { hnsw_validate_safe(opclass_oid) } else { true }'
expect_failure dead-delegation 'does not delegate to its safe function' "${dead_delegation}"

shadowed_delegation="$(make_fixture shadowed-delegation)"
replace_once "${shadowed_delegation}/hnsw_am_callbacks.rs" \
  '    self::hnsw_validate_safe(opclass_oid)' \
  $'    let hnsw_validate_safe = |_oid| true;\n    hnsw_validate_safe(opclass_oid)'
expect_failure shadowed-delegation 'does not delegate to its safe function' "${shadowed_delegation}"

glob_shadowed_delegation="$(make_fixture glob-shadowed-delegation)"
replace_once "${glob_shadowed_delegation}/hnsw_am_callbacks.rs" \
  '    self::hnsw_validate_safe(opclass_oid)' \
  $'    use decoy_validate_scope::*;\n    hnsw_validate_safe(opclass_oid)'
printf '%s\n' \
  'pub(super) fn hnsw_validate_decoy(_oid: pg_sys::Oid) -> bool { false }' \
  'mod decoy_validate_scope {' \
  '    pub(super) use super::hnsw_validate_decoy as hnsw_validate_safe;' \
  '}' \
  >>"${glob_shadowed_delegation}/hnsw_am_callbacks.rs"
rustfmt --edition 2024 --emit stdout "${glob_shadowed_delegation}/hnsw_am_callbacks.rs" >/dev/null
expect_failure glob-shadowed-delegation 'does not delegate to its safe function' \
  "${glob_shadowed_delegation}"

cfg_aliased_safe_inner="$(make_fixture cfg-aliased-safe-inner)"
replace_once "${cfg_aliased_safe_inner}/hnsw_am_callbacks.rs" \
  'fn hnsw_validate_safe(_opclass_oid: pg_sys::Oid) -> bool {' \
  $'#[cfg(any())]\nfn hnsw_validate_safe(_opclass_oid: pg_sys::Oid) -> bool {'
printf '%s\n' \
  'fn hnsw_validate_decoy(_oid: pg_sys::Oid) -> bool { false }' \
  'use self::hnsw_validate_decoy as hnsw_validate_safe;' \
  >>"${cfg_aliased_safe_inner}/hnsw_am_callbacks.rs"
rustfmt --edition 2024 --emit stdout "${cfg_aliased_safe_inner}/hnsw_am_callbacks.rs" >/dev/null
expect_failure cfg-aliased-safe-inner \
  'HNSW safe function must be an unconditional top-level definition' \
  "${cfg_aliased_safe_inner}"

unsafe_inner="$(make_fixture unsafe-inner)"
replace_once "${unsafe_inner}/hnsw_am_callbacks.rs" \
  'fn hnsw_validate_safe(_opclass_oid: pg_sys::Oid) -> bool {' \
  'unsafe fn hnsw_validate_safe(_opclass_oid: pg_sys::Oid) -> bool {'
expect_failure unsafe-inner 'pair with a safe function' "${unsafe_inner}"

inventory_drift="$(make_fixture inventory-drift)"
replace_once "${inventory_drift}/hnsw_am/callback_contract.rs" \
  'callback: "pgcontext_hnsw_validate"' \
  'callback: "pgcontext_hnsw_validate_drift"'
expect_failure inventory-drift 'unsafe callback source does not match' "${inventory_drift}"

routine_drift="$(make_fixture routine-drift)"
replace_once "${routine_drift}/hnsw_am.rs" \
  'amvalidate: Some(pgcontext_hnsw_validate),' \
  $'amvalidate: None,\n        // amvalidate: Some(pgcontext_hnsw_validate),'
expect_failure routine-drift 'IndexAmRoutine callback count mismatch' "${routine_drift}"

block_routine_decoy="$(make_fixture block-routine-decoy)"
replace_once "${block_routine_decoy}/hnsw_am.rs" \
  'amvalidate: Some(pgcontext_hnsw_validate),' \
  $'amvalidate: None,\n        /*\n        amvalidate: Some(pgcontext_hnsw_validate),\n        */'
rustfmt --edition 2024 --emit stdout "${block_routine_decoy}/hnsw_am.rs" >/dev/null
expect_failure block-routine-decoy 'IndexAmRoutine callback count mismatch' \
  "${block_routine_decoy}"

nested_routine_decoy="$(make_fixture nested-routine-decoy)"
replace_once "${nested_routine_decoy}/hnsw_am.rs" \
  'amvalidate: Some(pgcontext_hnsw_validate),' \
  $'amvalidate: {\n            struct Decoy<T> { amvalidate: T }\n            let _decoy = Decoy { amvalidate: Some(pgcontext_hnsw_validate) };\n            None\n        },'
rustfmt --edition 2024 --emit stdout "${nested_routine_decoy}/hnsw_am.rs" >/dev/null
expect_failure nested-routine-decoy 'IndexAmRoutine callback count mismatch' \
  "${nested_routine_decoy}"

computed_routine_callback="$(make_fixture computed-routine-callback)"
replace_once "${computed_routine_callback}/hnsw_am.rs" \
  'amvalidate: Some(pgcontext_hnsw_validate),' \
  $'amvalidate: Some({\n            // SAFETY: Negative fixture deliberately changes the callback ABI.\n            let pgcontext_hnsw_validate: unsafe extern "C-unwind" fn(pg_sys::Oid) -> bool = unsafe {\n                std::mem::transmute(decoy_validate_callback as fn(pg_sys::Oid) -> bool)\n            };\n            pgcontext_hnsw_validate\n        }),'
printf '%s\n' \
  'fn decoy_validate_callback(_oid: pg_sys::Oid) -> bool { false }' \
  >>"${computed_routine_callback}/hnsw_am.rs"
rustfmt --edition 2024 --emit stdout "${computed_routine_callback}/hnsw_am.rs" >/dev/null
expect_failure computed-routine-callback \
  'IndexAmRoutine callback field has unsupported value: amvalidate' \
  "${computed_routine_callback}"

rogue_callback="$(make_fixture rogue-callback)"
printf '%s\n' \
  '#[pg_guard]' \
  '// SAFETY: synthetic rogue callback for the negative fixture.' \
  'unsafe extern "C-unwind" fn pgcontext_hnsw_rogue() {}' \
  >>"${rogue_callback}/hnsw_am.rs"
expect_failure rogue-callback 'unsafe callback source does not match' "${rogue_callback}"

rogue_internal="$(make_fixture rogue-internal)"
printf '%s\n' \
  '// SAFETY: synthetic uninventoried helper for the negative fixture.' \
  'unsafe fn hnsw_uninventoried_raw_helper(_pointer: *mut u8) {}' \
  >>"${rogue_internal}/hnsw_am.rs"
expect_failure rogue-internal 'unsafe source count mismatch' "${rogue_internal}"

rogue_multiline_internal="$(make_fixture rogue-multiline-internal)"
printf '%s\n' \
  '// SAFETY: synthetic multiline uninventoried helper.' \
  'unsafe' \
  'fn hnsw_uninventoried_multiline_helper(_pointer: *mut u8) {}' \
  >>"${rogue_multiline_internal}/hnsw_am.rs"
rustfmt --edition 2024 --emit stdout "${rogue_multiline_internal}/hnsw_am.rs" >/dev/null
expect_failure rogue-multiline-internal 'unsafe source count mismatch' \
  "${rogue_multiline_internal}"

rogue_const_submodule="$(make_fixture rogue-const-submodule)"
printf '%s\n' \
  '// SAFETY: synthetic uninventoried const helper for the negative fixture.' \
  'pub(super) const unsafe fn hnsw_uninventoried_const_helper() {}' \
  >>"${rogue_const_submodule}/hnsw_am/storage.rs"
expect_failure rogue-const-submodule 'unsafe source count mismatch' "${rogue_const_submodule}"

rogue_impl="$(make_fixture rogue-impl)"
printf '%s\n' \
  'struct HnswSyntheticUnsafeImpl;' \
  'unsafe impl Send for HnswSyntheticUnsafeImpl {}' \
  >>"${rogue_impl}/hnsw_am.rs"
expect_failure rogue-impl 'unsafe source count mismatch' "${rogue_impl}"

rogue_multiline_impl="$(make_fixture rogue-multiline-impl)"
printf '%s\n' \
  'struct HnswSyntheticMultilineUnsafeImpl;' \
  'unsafe' \
  'impl Send for HnswSyntheticMultilineUnsafeImpl {}' \
  >>"${rogue_multiline_impl}/hnsw_am.rs"
rustfmt --edition 2024 --emit stdout "${rogue_multiline_impl}/hnsw_am.rs" >/dev/null
expect_failure rogue-multiline-impl 'unsafe source count mismatch' "${rogue_multiline_impl}"

rogue_same_line_impl="$(make_fixture rogue-same-line-impl)"
printf '%s\n' \
  'struct HnswSyntheticSameLineUnsafeImpl; unsafe impl Send for HnswSyntheticSameLineUnsafeImpl {}' \
  >>"${rogue_same_line_impl}/hnsw_am/storage.rs"
rustfmt --edition 2024 --emit stdout "${rogue_same_line_impl}/hnsw_am/storage.rs" >/dev/null
expect_failure rogue-same-line-impl 'unsafe source count mismatch' "${rogue_same_line_impl}"

macro_generated_unsafe="$(make_fixture macro-generated-unsafe)"
printf '%s\n' \
  'macro_rules! generate_hnsw_unsafe {' \
  '    ($name:ident) => { unsafe fn $name() {} };' \
  '}' \
  'generate_hnsw_unsafe!(hnsw_generated_unsafe);' \
  >>"${macro_generated_unsafe}/hnsw_am.rs"
rustfmt --edition 2024 --emit stdout "${macro_generated_unsafe}/hnsw_am.rs" >/dev/null
expect_failure macro-generated-unsafe \
  'HNSW checked sources must not define macros that can hide unsafe items' \
  "${macro_generated_unsafe}"

rogue_include="$(make_fixture rogue-include)"
printf '%s\n' 'include!("hnsw_rogue_include.rs");' >>"${rogue_include}/hnsw_am.rs"
printf '%s\n' \
  '// SAFETY: synthetic unsafe helper hidden behind an unreviewed include.' \
  'unsafe fn hnsw_uninventoried_from_include() {}' \
  >"${rogue_include}/hnsw_rogue_include.rs"
rustfmt --edition 2024 --emit stdout "${rogue_include}/hnsw_am.rs" >/dev/null
rustfmt --edition 2024 --emit stdout "${rogue_include}/hnsw_rogue_include.rs" >/dev/null
expect_failure rogue-include 'HNSW include! inventory mismatch' "${rogue_include}"

rogue_path_module="$(make_fixture rogue-path-module)"
printf '%s\n' \
  '#[path = "hnsw_rogue_path.rs"]' \
  'mod hnsw_rogue_path;' \
  >>"${rogue_path_module}/hnsw_am.rs"
printf '%s\n' \
  '// SAFETY: synthetic unsafe helper hidden behind a redirected module.' \
  'unsafe fn hnsw_uninventoried_from_path() {}' \
  >"${rogue_path_module}/hnsw_rogue_path.rs"
rustfmt --edition 2024 --emit stdout "${rogue_path_module}/hnsw_am.rs" >/dev/null
rustfmt --edition 2024 --emit stdout "${rogue_path_module}/hnsw_rogue_path.rs" >/dev/null
expect_failure rogue-path-module 'must not redirect modules with a path attribute' \
  "${rogue_path_module}"

rogue_cfg_attr_path="$(make_fixture rogue-cfg-attr-path)"
printf '%s\n' \
  '#[cfg_attr(all(), path = "hnsw_rogue_cfg_attr.rs")]' \
  'mod hnsw_rogue_cfg_attr;' \
  >>"${rogue_cfg_attr_path}/hnsw_am.rs"
printf '%s\n' \
  '// SAFETY: synthetic unsafe helper hidden behind cfg_attr path redirection.' \
  'unsafe fn hnsw_uninventoried_from_cfg_attr_path() {}' \
  >"${rogue_cfg_attr_path}/hnsw_rogue_cfg_attr.rs"
rustfmt --edition 2024 --emit stdout "${rogue_cfg_attr_path}/hnsw_am.rs" >/dev/null
rustfmt --edition 2024 --emit stdout "${rogue_cfg_attr_path}/hnsw_rogue_cfg_attr.rs" >/dev/null
expect_failure rogue-cfg-attr-path \
  'must not redirect modules with a path attribute' "${rogue_cfg_attr_path}"

nonprefix_callback="$(make_fixture nonprefix-callback)"
printf '%s\n' \
  '#[pg_guard]' \
  '// SAFETY: synthetic non-prefix callback for the negative fixture.' \
  'unsafe extern "C-unwind" fn unrelated_unsafe_callback() {}' \
  >>"${nonprefix_callback}/hnsw_am.rs"
expect_failure nonprefix-callback 'unsafe source count mismatch' "${nonprefix_callback}"

safe_export="$(make_fixture safe-export)"
printf '%s\n' \
  'extern "C-unwind" fn unexpected_hnsw_safe_export() {}' \
  >>"${safe_export}/hnsw_am.rs"
expect_failure safe-export 'unexpected safe HNSW C-unwind export inventory' "${safe_export}"

finfo_drift="$(make_fixture finfo-drift)"
replace_once "${finfo_drift}/hnsw_am.rs" \
  $'pub extern "C-unwind" fn pg_finfo_pgcontext_hnsw_handler() -> *const pg_sys::Pg_finfo_record {\n    &HNSW_HANDLER_FINFO\n}' \
  $'pub extern "C-unwind" fn pg_finfo_pgcontext_hnsw_handler() -> *const pg_sys::Pg_finfo_record {\n    // &HNSW_HANDLER_FINFO is intentionally only a decoy.\n    std::ptr::null()\n}'
expect_failure finfo-drift 'finfo exemption must return the immutable static record' "${finfo_drift}"

echo "HNSW callback guard smoke tests passed"
