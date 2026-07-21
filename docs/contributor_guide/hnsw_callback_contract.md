# HNSW Callback Boundary Contract

The experimental `pgcontext_hnsw` access method has one guarded Rust wrapper
for every PostgreSQL callback it installs. Each wrapper documents PostgreSQL's
caller contract, converts live raw pointers into private callback-local
capabilities, and immediately delegates to a safe Rust function. No callback
input pointer or reference is retained after that callback returns.

This inventory fixes the boundary that later HNSW storage, WAL, MVCC, and
VACUUM work must preserve. It does not claim that the current prototype has
implemented those later behaviors.

## Inventory

| PostgreSQL entrypoint | Role | Safe function | Borrowed input contract | Retained result/state |
|---|---|---|---|---|
| `pgcontext_hnsw_handler` | AM handler | `hnsw_handler_safe` | Function-call info is live for the call | PostgreSQL-allocated routine |
| `pgcontext_hnsw_build` | `ambuild` | `hnsw_build_safe` | Heap relation, index relation, and index info are live for the call | PostgreSQL-allocated build result |
| `pgcontext_hnsw_build_empty` | `ambuildempty` | `hnsw_build_empty_safe` | Index relation is live for the call | None |
| `pgcontext_hnsw_insert` | `aminsert` | `hnsw_insert_safe` | Relations, datum/null arrays, heap TID, and index info are call-bounded | None |
| `pgcontext_hnsw_insert_cleanup` | `aminsertcleanup` | `hnsw_insert_cleanup_safe` | Relation and index info are live for the call | None |
| `pgcontext_hnsw_bulk_delete` | `ambulkdelete` | `hnsw_bulk_delete_safe` | VACUUM info, optional stats, callback, and callback state are call-bounded | PostgreSQL-allocated stats |
| `pgcontext_hnsw_vacuum_cleanup` | `amvacuumcleanup` | `hnsw_vacuum_cleanup_safe` | VACUUM info and optional stats are call-bounded | PostgreSQL-allocated stats |
| `pgcontext_hnsw_options` | `amoptions` | `hnsw_options_safe` | Reloptions datum belongs to PostgreSQL for the call | PostgreSQL-allocated options |
| `pgcontext_hnsw_cost_estimate` | `amcostestimate` | `hnsw_cost_estimate_safe` | Planner/path inputs are borrowed; five distinct output slots are writable | None |
| `pgcontext_hnsw_validate` | `amvalidate` | `hnsw_validate_safe` | Operator-class OID is a copied scalar | None |
| `pgcontext_hnsw_begin_scan` | `ambeginscan` | `hnsw_begin_scan_safe` | Index relation is live and counts are nonnegative and within the one-key/one-order-by AM limit | One Rust scan state until `amendscan` or scan-context reset |
| `pgcontext_hnsw_rescan` | `amrescan` | `hnsw_rescan_safe` | Scan descriptor and optional arrays match bounded descriptor capacities | None |
| `pgcontext_hnsw_get_tuple` | `amgettuple` | `hnsw_get_tuple_safe` | Scan descriptor and its opaque state remain live | None |
| `pgcontext_hnsw_get_bitmap` | `amgetbitmap` | `hnsw_get_bitmap_safe` | Scan descriptor and TID bitmap are live for the call | None |
| `pgcontext_hnsw_end_scan` | `amendscan` | `hnsw_end_scan_safe` | Descriptor owns zero or one Rust opaque state | None; opaque state is consumed once |
| `pgcontext_hnsw_build_callback` | heap-build visitor | `hnsw_build_callback_safe` | Relation, tuple arrays, TID, and exclusive build state are live for the visitor call | None |

All 16 entrypoints use pgrx `#[pg_guard]`. The 14 `IndexAmRoutine` callbacks
are installed explicitly; the handler creates that routine and the heap-build
visitor is passed synchronously to PostgreSQL's table index build scan.

`pg_finfo_pgcontext_hnsw_handler` is the sole unguarded exported helper. It
returns an immutable static `Pg_finfo_record`, accepts no input, performs no
allocation, and cannot invoke Rust code that may panic.

## Pointer And Ownership Rules

- Guarded wrappers construct private, non-clonable `PgCallbackRef<T>` and
  `PgCallbackMut<T>` capabilities backed by `NonNull<T>`; null required inputs
  fail with a PostgreSQL error before delegation. The one exception is a
  mutable capability constructed immediately from a verified non-null result
  allocated by the safe VACUUM result allocator.
- Optional mutable callback pointers remain typed
  `Option<PgCallbackMut<T>>` values. Counted arrays use length-bearing
  `PgCallbackSlice<T>` capabilities.
- Safe functions may use a capability only within the callback. They cannot
  return a borrow tied to it or store it in scan/build state.
- Mutable access requires the wrapper's PostgreSQL contract to grant exclusive
  access. Writable planner outputs are represented by separate capabilities.
- The scan's Rust state is the only retained Rust allocation. `ambeginscan`
  registers a drop slot with the PostgreSQL memory context that owns the scan
  descriptor and exposes that slot through `opaque`. `amrescan` only resets
  the state. Normal `amendscan` takes and drops the state once and clears
  `opaque`; ERROR or cancellation instead drops it through the memory-context
  reset callback. The callback later sees an empty slot after normal cleanup,
  so both paths are exactly-once.
- Datum arrays, scan keys, heap TIDs, relation pointers, VACUUM callback state,
  and build-visitor pointers are never retained.

Externally supplied counts are converted and bounded before allocation or
copying. This single-column AM accepts at most one scan key and one order-by
key. Scan order-by allocation uses checked multiplication. Rescan sources are
length-bearing callback-local slices; positive counts require non-null arrays,
destination capacities are rechecked, and the single value is staged in owned
stack storage before writing its destination. Later
live VACUUM work must continue to invoke deletion
callbacks only after releasing graph locks, as specified by the
[HNSW Storage Mutation Contract](./hnsw_storage_contract.md).

## Review And Verification

The executable inventory in `hnsw_am/callback_contract.rs` must remain in sync
with this table. Focused unit tests prove inventory uniqueness, the 14-AM
callback count, nonempty borrow contracts, safe-function pairing, callback
allocation overflow rejection, rescan capacity rejection, and bounded pointer
access. `scripts/check-hnsw-callback-guards.sh` invokes a dependency-free,
token-aware Rust source scanner and mechanically reconciles that inventory with
every unsafe `C-unwind` definition, the 14 direct callbacks installed in
`IndexAmRoutine`, each `#[pg_guard]`, its local line-comment `SAFETY:` contract,
its safe-function definition, and the wrapper's unique final-statement
`self::` delegation. Direct module qualification prevents a block-local import
or binding from redirecting a callback. Its adversarial shell suite includes
rustfmt-validated regressions for nested/block-comment and literal decoys,
multiline and macro-generated unsafe definitions, nested or computed routine
callbacks, and safe-function shadowing; it also rejects dead-code delegation,
finfo drift, and routine/inventory drift. Local macro definitions are forbidden
in this checked surface because expansion could otherwise hide unsafe items.

`hnsw_am/unsafe_inventory.data` separately records every unsafe function,
unsafe extern, and unsafe impl in the main module, all submodules, and the
included page-storage adapter. The same checker compares that pinned manifest
to all HNSW Rust sources and owner-qualifies unsafe methods, so adding, removing,
or moving any unsafe definition requires an explicit reason-bearing inventory
change. The current pinned baseline is 55 unsafe items.

Use the normal PostgreSQL-adapter gates after changing this boundary:

```sh
PG_CONFIG=/opt/homebrew/opt/postgresql@17/bin/pg_config \
  cargo test -p context-pg --no-default-features --features pg17 hnsw_am::
PG_CONFIG=/opt/homebrew/opt/postgresql@17/bin/pg_config \
  cargo clippy -p context-pg --no-default-features --features pg17 \
  --lib --tests -- -D warnings -A clippy::cast_precision_loss \
  -A clippy::cast_possible_truncation -A clippy::cast_possible_wrap
scripts/check-unsafe-safety-comments.sh
scripts/check-hnsw-callback-guards.sh
tests/shell/check_hnsw_callback_guards_smoke.sh
```
