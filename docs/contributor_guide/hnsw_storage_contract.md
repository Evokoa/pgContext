# HNSW Storage Mutation Contract

This document fixes the version-two logical page, insertion, recovery-state,
and lock-order contract that PostgreSQL page/WAL adapters must implement. It is
design input, not evidence that the current experimental access method is
durable or ready to serve production queries.

## Version-Two Page Envelope

The legacy prototype metapage calls itself version 1 while vector records are
unversioned, native-endian `repr(C)` data. Version 2 never reinterprets those
bytes. Zero, version 1, and future versions fail closed as rebuild-required.

All numeric version-2 payload fields use little-endian encoding. PostgreSQL
retains its normal page header and line pointers; each pgContext page item
starts with this 32-byte envelope:

| Offset | Width | Field | Contract |
|---:|---:|---|---|
| 0 | 4 | magic | ASCII `PGH2` |
| 4 | 2 | format_version | `2` |
| 6 | 1 | page_kind | meta `1`, directory `2`, node `3`, adjacency `4`, mutation descriptor `5` |
| 7 | 1 | flags | must be zero until assigned by a later version |
| 8 | 2 | header_bytes | `32` |
| 10 | 6 | reserved | must encode as zero and decode as zero |
| 16 | 8 | generation | published generation this item belongs to |
| 24 | 8 | mutation_id | zero for published data; nonzero for pending work |

Page/item roles after the envelope are:

| Kind | Required logical fields | Lookup/use |
|---|---|---|
| Meta | availability `u8`, root level `u8`, reserved `u16`, dimensions `u32`, next node id `u64`, structural published node count `u64`, tombstone count `u64`, entry node id `u64` (`u64::MAX` means none), last-published mutation id `u64` (zero means none), directory root page `u64`, descriptor-directory root page `u64`, pending count `u16`, pending capacity `u16` (`128`), then 128 `(mutation id u64, node id u64)` slots where mutation id zero means empty | Block zero only; reader-visible state and its publication identity are replaced last. Tombstone count never exceeds structural node count. The 2,048-byte reservation region is bounded and `next node id` is never inferred from published count. |
| Directory | key kind `u8` (node `1`, adjacency `2`, mutation descriptor `3`, mutation entry `4`), reserved `[u8;7]`, generation `u64`, node or mutation id `u64`, layer or entry ordinal `u16`, reserved `[u8;6]`, target page `u64`, target slot `u16`, reserved `[u8;6]`, record revision `u64` | Node and adjacency lookup selects the greatest stored generation no newer than the published metapage generation. Descriptor keys use generation zero. The bounded directory also maps mutation id to its header and `(mutation id, ordinal)` to a complete entry. A lookup visits at most 8 directory pages; excess depth is rebuild-required. |
| Node | node id `u64`, graph record token `u64`, record revision `u64`, heap block `u32`, heap offset `u16`, publication state `u8`, tombstone reason `u8`, tombstone epoch `u64`, layer count `u8`, reserved `[u8;3]`, dimensions `u32`, then `dimensions` finite `f32` values | One incremental scoring payload. Heap offset is nonzero. Publication is unpublished `0`, ready `1`, or tombstoned `2`; other values are rebuild-required. Tombstone reason is zero and epoch is zero unless state is tombstoned; v2 reason `1` means PostgreSQL VACUUM declared the TID dead to all supported snapshots. Tombstones retain vector/topology identity for traversal but are never answer candidates. |
| Adjacency | node id `u64`, layer `u16`, reserved `u16`, neighbor count `u32`, record revision `u64`, then `neighbor count` node ids as `u64` | One complete bounded layer replacement. Partial neighbor-vector updates are forbidden. |
| Mutation descriptor | Header: mutation id `u64`, node id `u64`, graph record token `u64`, descriptor revision `u64`, phase `u8` (prepared `0`, appended `1`, outbound `2`, rewire `3`, mark-ready `4`, ready-to-publish `5`, published `6`), reserved `u8`, outbound total/completed `u16` each, rewire total/completed `u16` each, reserved `u16`, then complete expected and target published states. Entry: kind `u8` (outbound `1`, rewire `2`), has-expected-revision `u8`, ordinal `u16`, layer `u16`, neighbor count `u16`, target node `u64`, expected revision `u64` (ignored when its flag is zero), then the complete bounded neighbor list. | A fixed descriptor-directory root is created during relation initialization. Header and entry records may occupy multiple descriptor pages; the bounded directory addresses each entry without a scan. An entry is never a partial neighbor slice. |

Both directory roots are initialized before the relation accepts insertions and
their metapage locators do not swing during an insertion. Directory growth
initializes unreachable pages first and links them through bounded data-page
actions; a semantic mutation is replanned before locks if its final page set
would exceed the Generic-WAL limit.

A complete published-state value contains generation `u64`, structural node
count `u64`, tombstone count `u64`, has-entry `u8`, has-dimensions `u8`, format
version `u16`, dimensions `u32`, entry node id `u64`, and last-published
mutation id `u64`; absent optional values
use their flags or the reserved zero mutation identity rather than stealing a
valid node/revision value. Reserved bits and bytes must remain zero. Nonempty
published state requires a nonzero last-published mutation identity, making an
exact target-state replay distinguishable from a concurrent insert that chose
the same root. Tombstone count is at most structural node count; their
difference is the candidate-bearing count. Insert publication increments the
structural count and preserves tombstone count. Tombstone publication preserves
the structural count and increments tombstone count exactly once.

The durability codec child owns byte encoders/decoders, record-length checks,
checksums, alignment tests, and corruption fuzzing. It may add validation but
must not change these version-2 meanings without choosing a new format version.

The physical adapter's bounded adjacency item codec carries a `u32` node id,
`u32` layer, `u32` neighbor count, four reserved zero bytes, then exactly that
many `u32` node ids. It rejects invalid layers, over-limit counts, reserved
bytes, truncation, and trailing bytes before returning an owned layer.

Directory locators use a fixed 56-byte version-two item: key kind, generation,
identity, layer/ordinal, target page/slot, and record revision, with every gap
reserved as zero. The live insertion adapter will use these locators for
versioned rewires and mutation descriptors.

Typed append returns the committed physical page and line-pointer location
only after `GenericXLogFinish`, so a subsequent directory item can name an
actual durable target rather than a speculative buffer address.

Readers process append-only adjacency revisions in physical order and retain
the last complete `(node, layer)` item. This allows a later rewire to supersede
an earlier complete layer without exposing a partial neighbor vector.
If a crash leaves a node revision pointing at an absent later node record,
recovery discards only those missing-node edges and falls back to exact scoring
when the remaining topology is not restorable.

Test builds expose named physical failpoints before/after page initialization,
append, rewiring, `GenericXLogFinish`, and metapage publication. Crash tests
select these stable names; production builds compile the hooks out.

`tests/heavy/hnsw_wal_crash_replay.sh` performs an immediate PostgreSQL stop,
restart, forced index plan, and exact-order oracle for each named boundary.
Set `PGRX_DATA_DIR` to exercise PostgreSQL's immediate-stop mode; the local
developer fallback uses the configured pgrx stop/start lifecycle.
The harness installs with `pg_test` and selects each hook through
`pgcontext.test_set_hnsw_physical_failpoint(text)`, an API absent from normal
extension builds.

`tests/heavy/hnsw_replica_promotion.sh` uses a local replication role and slot,
`pg_basebackup -R`, streaming catch-up, and `pg_ctl promote`; the promoted
cluster then compares a forced HNSW order to the exact oracle.
It removes its temporary replication role, slot, and `pg_hba.conf` rule during
cleanup. The bounded `before_append` replay and standby/promotion paths are covered by
the crash-recovery test suite; extended named-boundary coverage is planned.
The widened fixture also exercises `before_page_initialization` by forcing a
new typed data page.

`scripts/run-pg17-recovery-report.sh --pg-major 17 --plan` is the canonical,
side-effect-free recovery manifest. Its `--dry-run` mode writes the same crash
and standby rows without touching PostgreSQL; `--approve` is explicit, requires
the primary pgrx data directory, and records complete approval only when every
non-skippable row passes from a clean worktree.

HNSW indexes inherit PostgreSQL relation persistence: logged tables produce
`p` indexes, unlogged tables produce `u` indexes, and temporary tables produce
session-local `t` indexes. `hnsw_relation_kinds.sh` pins all three catalog
values and a forced ordered HNSW lookup; only logged indexes participate in the
WAL/standby evidence above.

## Insertion And Publication

Node-id reservation advances a separate never-reused watermark under a
standalone allocator lock on block zero. The update is an expected-watermark
compare-and-swap and is idempotent by nonzero mutation id. A stale prepared
reservation retries after releasing the lock. At most 128 pending mutation
reservations are retained in the metapage's fixed 2,048-byte region, so reservation does not
allocate while locked. Mutation ids are monotone and never reused; their pending
slot is released only after publication or explicit repair discard. An aborted
or interrupted insertion may leave a
hole; it never causes a node id to be reused and never increments the published
node count by itself.

| Order | Semantic step | Reader rule |
|---:|---|---|
| 1 | Prepare all bounded encodings and expected revisions with no lock held. | No reader-visible change. |
| 2 | Under the standalone allocator lock, reserve node id with expected watermark and persist the mutation-id mapping; then release. | No reader-visible count/root change. |
| 3 | Append node as unpublished for the next generation. | Ignore the node and any edge targeting it. |
| 4 | Write every complete outbound `(node, layer)` adjacency record. | Continue ignoring the unpublished node. |
| 5 | Append complete next-generation adjacency revisions for existing nodes using expected current revisions. Never overwrite the published-generation record in place. | Directory lookup at the old published generation still returns the old complete layer. |
| 6 | Mark the node ready after every required rewire succeeds. | The old published generation/root remains authoritative. |
| 7 | Replace published metapage state last, comparing the complete expected reader state. Exact target-state replay for the same mutation is a no-op; any other state is a conflict. | New count/root/generation become visible together. |

The old root and its versioned node/adjacency lookup remain unchanged through
step 5. Root publication rejects a missing, unpublished, wrong-generation, or
corrupt node. Multiple mutation descriptors may coexist; there is no global
assumption that only one insertion can be in flight. Old generations are
reclaimed only by the later MVCC/VACUUM policy after no supported reader can use
them.

## MVCC, Tombstones, And Source Recheck

Ready means structurally complete, not visible to the active heap snapshot. An
unpublished node is ignored for both traversal and answers. A ready node is a
topology connector and candidate source only; a tombstone remains a connector
but can never be returned. Consequently, a structurally published node left by
an aborted transaction is harmless: a missing or invisible source row excludes
it, and a later VACUUM callback may tombstone it.

`HnswHeapTid { block u32, offset u16 }`, graph record token, graph node ID,
mutation ID, legacy algorithm point ID, and logical `context_core::PointId` are
distinct identities. No TID-to-PointId cast exists. A collection query obtains
its logical point mapping, ACL/RLS/predicate result, current source vector, and
finite exact score from the authoritative PostgreSQL row under the statement
snapshot. The ANN score is candidate-ordering input only and is never the final
public score. A raw access-method scan likewise carries a physical TID only;
its stored score is valid only while the VACUUM/TID-reuse invariant proves that
the binding still names that indexed row version. Merely setting
`xs_recheckorderby` is not a substitute because PostgreSQL requires recheck
order values to be valid lower bounds.

TID reuse is fenced: before a reused `(block, offset)` can bind a new graph node,
every older binding for that TID must have a published tombstone. The old node
may remain in adjacency lists for routing, while only the fresh node can pass
source recheck. Node IDs and mutation IDs are never reused.

A tombstone mutation binds mutation ID, node ID, graph record token, physical
TID, expected node revision, previous complete published state, target
generation, and target node revision. It proceeds in two reader-safe steps:

1. Append the tombstoned node version and generation-aware locator using exact
   record-revision compare-and-replace. The previous published generation
   remains authoritative.
2. Publish block-zero metadata alone, preserving structural node count/root and
   incrementing tombstone count once with the tombstone mutation as
   `last_mutation_id`.

Exact store and publication replay are idempotent. Changed node/record identity,
revision, or complete published state is a typed conflict that releases locks
and replans. An interrupted tombstone store is an unreachable next-generation
orphan; a tombstoned entry point remains legal because it is traversal-only.

VACUUM uses a fixed-capacity two-phase protocol. With graph locks released, it
copies at most 64 node/TID/revision identities, invokes
`IndexBulkDeleteCallback`, and deduplicates callback-true TIDs. Callback false
keeps the node ready; callback true is PostgreSQL's dead-to-all-supported-
snapshots authority. Only after the callback phase is sealed may the adapter
prepare tombstone transitions, reacquire pages in canonical order, revalidate
revisions, and later apply Generic WAL. Already-tombstoned nodes do not change
counts. The batch flushes and checks interrupts before collecting more; there
is no unbounded allocation. `amvacuumcleanup` never invokes the deletion
callback or invents removals. `tuples_removed` counts newly published
tombstones cumulatively, `num_index_tuples` counts candidate-bearing nodes, and
`estimated_count` mirrors `IndexVacuumInfo::estimated_count` once the live
adapter is implemented.

## PostgreSQL Generic-WAL Units

Version 1 uses PostgreSQL Generic WAL; it does not define or require a custom
resource manager or shared-preload library. PostgreSQL 17 permits at most four
registered buffers in one Generic WAL record. The context-pg planner therefore
stores 1..=4 unique page actions in fixed-capacity memory and requires their
registration order to match strictly ascending lock order. Block-zero allocator
and publication actions are always standalone.

Each data-page action also carries its exact logical write: node/adjacency id,
layer, target generation, append-versus-revision mode, expected revision where
applicable, descriptor expected/target revisions, and the directory keys being
inserted or removed. Tombstone writes additionally bind graph record token and
expected/target node revision. Node, adjacency, and tombstone locator writes
are insert-only for the target generation. A rewire or tombstone plan whose
record or locator action names the currently published generation is invalid
before buffer acquisition; it cannot masquerade as a next-generation append.

One registered directory page may carry the two locator changes owned by the
same atomic unit: append inserts the versioned node locator plus descriptor
header locator, while a topology unit inserts the versioned adjacency locator
plus immutable descriptor-entry locator. Both keys and their insert-only mode
are part of the typed page action; they are not inferred by the codec.

The future physical adapter must modify only the shadow pages returned by
`GenericXLogRegisterBuffer`, keep each buffer exclusively locked from before
registration through `GenericXLogFinish`, use `GENERIC_XLOG_FULL_IMAGE` for a
new page, and let `GenericXLogFinish` dirty pages and set LSNs. A failed plan is
aborted before finish; successful finish applies every registered page in that
unit. These requirements follow PostgreSQL's
[Generic WAL contract](https://www.postgresql.org/docs/17/generic-wal.html).

`HnswWalCriticalPlan` is the enforced pre-finish typestate boundary. It consumes
an already allocated semantic unit, revalidates the fixed 1..=4 page set and
lock order, selects a static diagnostic without runtime formatting, and freezes
the page actions into fixed-capacity storage. Its fallible staging closure is
called exactly once per frozen page in lock order; any adapter error or named
preparation failpoint returns before a finish permit exists, while Generic WAL
can still be aborted normally. Only complete staging yields the linear
`HnswWalFinishPermit`. That permit intentionally has no methods, formatting
traits, callback, iterator, allocation, or `Result` surface. The later live
adapter must add the sole completion path inside the same module immediately
beside its direct `GenericXLogFinish` call. PostgreSQL performs the actual
critical section internally in `GenericXLogFinish`; no Rust fallible work is
allowed in or after that call before the permit is consumed.

| Unit | Registered page roles | Published visibility after finish |
|---|---|---|
| Reserve node id | block-zero allocator state only | None. Expected watermark and mutation-id reservation advance together. |
| Initialize page | relation extension serialization with no existing graph page held, followed by exactly one exclusively locked new nonzero directory, node, adjacency, or descriptor page as a full image | None. The page remains unreachable. |
| Append unpublished node | exactly one node page, exactly one descriptor-header page, and at most one locator page | None. Node record and exact descriptor creation are one unit. |
| Write outbound layer | exactly one adjacency page, one descriptor-header page, one immutable complete-entry page, and at most one locator page | None. The entry payload and exact header CAS are bound to the same unit; layers are never combined into an unbounded record. |
| Replace neighbor layer | exactly one next-generation adjacency page, one descriptor-header page, one immutable complete-entry page, and at most one locator page | None. One complete expected-revision rewire and exact header CAS are a unit; multiple rewires remain an interruptible sequence without modifying old-generation adjacency. |
| Mark node ready | exactly one node page and one descriptor-header page | None. The ready record and exact header CAS are one unit; old metadata remains authoritative. |
| Publish root | block zero only | Complete expected-state compare-and-replace publishes generation, count, and root last and clears the metapage reservation slot. |
| Store tombstone | exactly one node page and one generation-aware locator page | None. The exact node/record identity and expected-to-target revision are stored for the target generation while the prior generation remains visible. |
| Publish tombstone | block zero only | Complete expected-state compare-and-replace preserves structural count/root, increments tombstone count once, and records the tombstone mutation identity. |
| Release/discard or descriptor cleanup | block-zero allocator alone, or descriptor plus locator data pages in a separate unit | None. Stale descriptor records after successful publication are harmless and cleanup is idempotent. |

Every progress unit carries either descriptor creation or an exact
expected-header-to-target-header revision transition. Outbound and rewire units
also carry the immutable complete descriptor entry they persist. Completion is
exposed to the state machine only after the future physical adapter reports a
successful `GenericXLogFinish`.

If append, layer, descriptor, or locator placement would require five pages,
the planner rejects it before lock acquisition. Page initialization and
directory-link preparation may be split into deterministic unreachable-page
units, but a node append or complete layer replacement is never truncated or
silently divided. The insertion state advances only after the corresponding
`GenericXLogFinish` succeeds.

## Prefix And Recovery Classification

| Applied prefix | Persistent interpretation | Required action |
|---|---|---|
| Prepared only | no page change | ready |
| Unpublished node only | structurally ignorable orphan | repair append or discard |
| Some outbound layers | unpublished node remains ignorable | repair-required: resume/discard outbound work |
| Some next-generation rewires | old-generation directory entries and complete layers remain authoritative | repair-required: resume revision-checked rewires or discard the unpublished generation |
| Ready node, old metapage | old root/count/generation remain visible | repair-required: validate then publish or discard |
| New metapage published | insertion complete | ready |
| Tombstone node version stored, old metapage | harmless unreachable orphan; old generation and answer eligibility remain authoritative | remain ready on the old generation; a later callback/reclamation pass may supersede or discard the orphan |
| Tombstone metapage published | old node remains traversal-only; structural count is unchanged | ready; callback replay does not double-count |
| Unsupported version/kind, missing descriptor, invalid published metadata/root, or corrupt required published page | no trustworthy repair prefix | rebuild-required from PostgreSQL source of truth |

Discoverable insertion-descriptor repair-required states freeze new graph
writes. An unreachable tombstone orphan has no pending-marker meaning and does
not freeze writes. Readers may use only the previously published generation
after validating it. Rebuild-required fails closed. These states never assume
that transaction rollback removes physical index remnants.

## Lock Protocol

1. Allocation/encoding/validation completes before lock acquisition.
2. Node-id reservation takes the allocator lock on block zero alone, compares
   and advances the persisted watermark, records the nonzero mutation id, and
   releases before any extension/data action.
3. Relation extension is a standalone action with no graph buffer lock held.
4. Existing directory/node/adjacency/descriptor pages are acquired by strictly
   ascending nonzero page id, with no duplicates, and released in reverse
   order. A semantic lock plan may inspect at most 8 pages, but one PG17 Generic
   WAL unit registers and mutates at most 4.
5. Metapage publication locks block zero alone after all data actions. The
   metapage lock is never held while extending or acquiring a data page.
6. If a target set, allocator watermark, complete published state, or record revision changes,
   release locks and replan; never
   upgrade in place.
7. No SPI, heap callback, formatting, allocation, or user-defined code runs
   while these locks are held.
8. VACUUM copies a bounded identity batch, releases every graph lock, invokes
   the deletion callback, seals the callback phase, and only then reacquires
   pages for revision validation and WAL. Callback false and long-snapshot
   visibility never create tombstones.

The typed unit model fixes action boundaries but does not emit WAL. Physical
codecs, live `GenericXLog*` calls, failpoint-driven crash replay, standby
correctness, live callback consumption, forced TID-reuse/HOT/savepoint/long-
snapshot coverage, accurate bloat diagnostics, relation-kind behavior, and
proof that current callbacks obey this protocol remain later gates.
