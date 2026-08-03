// Compaction pg_tests (P2-S4): pgcontext.compact() rebuilds an index from
// its own pages and drains the delta segment.
//
// Every result assertion compares the index against an oracle forced off the
// index (enable_indexscan = off, enable_seqscan = on). Comparing the HNSW
// path against itself passes whether or not compaction preserved anything,
// which is how the S3 delta tests shipped unable to fail.
//
// Two things these tests deliberately do not claim. Compaction cannot
// reclaim deleted rows here, because only VACUUM writes the tombstones that
// retire them and VACUUM cannot run inside a #[pg_test] transaction; that
// path lives in `scripts/check-hnsw-vacuum.sh`. And no assertion below
// discriminates the metapage's `base_start_block` bound: `graph_nodes`
// independently caps the packed graph, so records read from a superseded
// base are discarded before a query can observe them. The bound is
// defense-in-depth, and its one deterministic observable — folding a
// shrunken base twice — needs the VACUUM the harness cannot run.

/// Shares the S3 probe fixture: a distinct prime per dimension and a modulus
/// above the row count, so every row's vector is unique and nearest-neighbour
/// assertions are not really asserting a tie-break.
const COMPACT_PROBE_VECTOR: &str = "(SELECT '[' || string_agg(((n * p) % 211 + 1)::text, ',' \
     ORDER BY ord) || ']' \
     FROM unnest(ARRAY[13,29,41,53,67,79,89,101]) WITH ORDINALITY AS primes(p, ord))::vector";

fn compact_probe_ids(sql: &str) -> Vec<i64> {
    Spi::connect(|client| {
        let result = client.select(sql, None, &[]).expect("probe query should run");
        let mut ids = Vec::new();
        for row in result {
            ids.push(row.get::<i64>(1).unwrap().unwrap_or_default());
        }
        Ok::<_, spi::Error>(ids)
    })
    .expect("probe rows should decode")
}

/// Runs `sql` with the planner forced off the index, so the result is a real
/// oracle rather than the same HNSW plan under another name.
fn compact_probe_exact_ids(sql: &str) -> Vec<i64> {
    Spi::run("SET enable_indexscan = off").expect("indexscan off should apply");
    Spi::run("SET enable_seqscan = on").expect("seqscan on should apply");
    let ids = compact_probe_ids(sql);
    Spi::run("SET enable_indexscan = on").expect("indexscan on should apply");
    Spi::run("SET enable_seqscan = off").expect("seqscan off should apply");
    ids
}

fn compact(index: &str) -> (i64, i64, i64) {
    Spi::connect(|client| {
        let result = client
            .select(
                &format!("SELECT live_rows, base_records_read, delta_records_drained \
                          FROM pgcontext.compact('{index}'::regclass)"),
                None,
                &[],
            )
            .expect("compact should run");
        let mut rows = Vec::new();
        for row in result {
            rows.push((
                row.get::<i64>(1).unwrap().unwrap_or_default(),
                row.get::<i64>(2).unwrap().unwrap_or_default(),
                row.get::<i64>(3).unwrap().unwrap_or_default(),
            ));
        }
        Ok::<_, spi::Error>(rows)
    })
    .expect("compact report should decode")
    .into_iter()
    .next()
    .expect("compact returns exactly one report row")
}

#[pg_test]
fn hnsw_compaction_preserves_results_and_drains_the_delta() {
    Spi::run(
        "CREATE TABLE compact_probe (id bigint PRIMARY KEY, \
         embedding vector(8) NOT NULL)",
    )
    .expect("compact probe table should be created");
    Spi::run(&format!(
        "INSERT INTO compact_probe SELECT n, {COMPACT_PROBE_VECTOR} \
           FROM generate_series(1, 120) n"
    ))
    .expect("compact probe base rows should insert");
    Spi::run(
        "CREATE INDEX compact_probe_hnsw ON compact_probe \
         USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops)",
    )
    .expect("compact probe index should build");
    Spi::run("SET enable_seqscan = off").expect("seqscan off should apply");

    // These land in the delta segment, not the base graph.
    Spi::run(&format!(
        "INSERT INTO compact_probe SELECT n, {COMPACT_PROBE_VECTOR} \
           FROM generate_series(121, 135) n"
    ))
    .expect("compact probe delta rows should insert");

    let top_k = "SELECT id FROM compact_probe \
         ORDER BY embedding OPERATOR(pgcontext.<=>) \
         (SELECT embedding FROM compact_probe WHERE id = 130) \
         LIMIT 10";
    let exact = compact_probe_exact_ids(top_k);
    let before = compact_probe_ids(top_k);
    assert_eq!(
        before, exact,
        "fixture check: the pre-compaction index must already match the oracle"
    );

    let (live_rows, _base_read, delta_drained) = compact("compact_probe_hnsw");
    assert_eq!(live_rows, 135, "every live row must survive compaction");
    assert!(
        delta_drained >= 15,
        "compaction must consume the delta records, saw {delta_drained}"
    );

    let after = compact_probe_ids(top_k);
    assert_eq!(
        after, exact,
        "compaction must not change which rows the index returns"
    );

    // The delta is empty again, so the fast append path is available: a row
    // inserted now is absorbed rather than spliced inline.
    let records_before = read_stat("delta_segment_records");
    Spi::run(&format!(
        "INSERT INTO compact_probe SELECT n, {COMPACT_PROBE_VECTOR} \
           FROM generate_series(136, 136) n"
    ))
    .expect("post-compaction insert should apply");
    assert_eq!(
        read_stat("delta_segment_records") - records_before,
        1,
        "compaction must reopen the delta segment for fast appends"
    );

    Spi::run("RESET enable_seqscan").expect("seqscan should reset");
}

#[pg_test]
fn hnsw_compaction_keeps_deleted_rows_out_of_results_until_vacuum() {
    Spi::run(
        "CREATE TABLE compact_delete_probe (id bigint PRIMARY KEY, \
         embedding vector(8) NOT NULL)",
    )
    .expect("compact delete probe table should be created");
    Spi::run(&format!(
        "INSERT INTO compact_delete_probe SELECT n, {COMPACT_PROBE_VECTOR} \
           FROM generate_series(1, 60) n"
    ))
    .expect("compact delete probe rows should insert");
    Spi::run(
        "CREATE INDEX compact_delete_probe_hnsw ON compact_delete_probe \
         USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops)",
    )
    .expect("compact delete probe index should build");
    Spi::run("SET enable_seqscan = off").expect("seqscan off should apply");

    Spi::run("DELETE FROM compact_delete_probe WHERE id % 4 = 0")
        .expect("delete should apply");

    // Compaction rebuilds from index pages, and a plain DELETE leaves no mark
    // on them: only VACUUM appends the tombstones that retire a row. So the
    // compacted graph still carries all 60 nodes, and the deleted rows stay
    // out of results the same way they did before — MVCC recheck against the
    // heap. Reclaiming them requires VACUUM first, which is covered by
    // `scripts/check-hnsw-vacuum.sh` because VACUUM cannot run in the
    // transaction wrapping a #[pg_test].
    let (live_rows, _base_read, _delta) = compact("compact_delete_probe_hnsw");
    assert_eq!(
        live_rows, 60,
        "without a preceding VACUUM the index has no tombstone for a deleted \
         row, so compaction cannot drop it"
    );

    let top_k = "SELECT id FROM compact_delete_probe \
         ORDER BY embedding OPERATOR(pgcontext.<=>) \
         (SELECT embedding FROM compact_delete_probe WHERE id = 33) \
         LIMIT 10";
    let after = compact_probe_ids(top_k);
    let exact = compact_probe_exact_ids(top_k);
    assert_eq!(after, exact, "compaction must agree with the exact oracle");
    assert!(
        after.iter().all(|id| id % 4 != 0),
        "a deleted row must not resurface after compaction, saw {after:?}"
    );

    // Drain the whole index rather than a top-10 window, so a row returned
    // twice is visible: each live row must appear exactly once and no deleted
    // row at all.
    let full_scan = "SELECT id FROM compact_delete_probe \
         ORDER BY embedding OPERATOR(pgcontext.<=>) \
         (SELECT embedding FROM compact_delete_probe WHERE id = 33) \
         LIMIT 1000";
    let drained = compact_probe_ids(full_scan);
    let mut unique = drained.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        drained.len(),
        unique.len(),
        "a full scan must return each row once, saw {drained:?}"
    );
    assert_eq!(
        unique.len(),
        45,
        "the index must serve exactly the undeleted rows after compaction"
    );

    // A second compaction must fold exactly the base the first published —
    // not that base plus the one it superseded.
    let (_live, second_base_read, _delta) = compact("compact_delete_probe_hnsw");
    assert_eq!(
        second_base_read, live_rows,
        "the second compaction must read only the base the first published"
    );

    Spi::run("RESET enable_seqscan").expect("seqscan should reset");
}

#[pg_test]
fn hnsw_compaction_is_repeatable_and_handles_an_untouched_index() {
    Spi::run(
        "CREATE TABLE compact_idempotent_probe (id bigint PRIMARY KEY, \
         embedding vector(8) NOT NULL)",
    )
    .expect("compact idempotent probe table should be created");
    Spi::run(&format!(
        "INSERT INTO compact_idempotent_probe SELECT n, {COMPACT_PROBE_VECTOR} \
           FROM generate_series(1, 40) n"
    ))
    .expect("compact idempotent probe rows should insert");
    Spi::run(
        "CREATE INDEX compact_idempotent_probe_hnsw ON compact_idempotent_probe \
         USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops)",
    )
    .expect("compact idempotent probe index should build");
    Spi::run("SET enable_seqscan = off").expect("seqscan off should apply");

    let top_k = "SELECT id FROM compact_idempotent_probe \
         ORDER BY embedding OPERATOR(pgcontext.<=>) \
         (SELECT embedding FROM compact_idempotent_probe WHERE id = 7) \
         LIMIT 5";
    let exact = compact_probe_exact_ids(top_k);

    // Compacting an index with nothing in its delta is legal and must be a
    // faithful rebuild, not a no-op that leaves a stale base published.
    let (first_live, _, first_delta) = compact("compact_idempotent_probe_hnsw");
    assert_eq!(first_live, 40);
    assert_eq!(first_delta, 0, "an untouched index has an empty delta");
    assert_eq!(compact_probe_ids(top_k), exact);

    // A second compaction reads the base the first one published; getting
    // the same live set proves the new base_start_block bound is respected
    // and the superseded pages are not read back.
    let (second_live, second_base_read, _) = compact("compact_idempotent_probe_hnsw");
    assert_eq!(
        second_live, 40,
        "compacting twice must not duplicate or lose rows"
    );
    assert_eq!(
        second_base_read, 40,
        "the second compaction must read only the base the first published, \
         not that base plus the one it superseded"
    );
    assert_eq!(compact_probe_ids(top_k), exact);

    Spi::run("RESET enable_seqscan").expect("seqscan should reset");
}

#[pg_test]
#[should_panic(expected = "requires a pgcontext_hnsw index relation")]
fn hnsw_compaction_rejects_a_table() {
    Spi::run("CREATE TABLE compact_not_an_index (id bigint)")
        .expect("table should be created");
    let _ = compact("compact_not_an_index");
}

// Bounded segmented rotation (C3): inserts always use the active delta;
// reaching its limit rotates one immutable segment and a saturated directory
// compacts one adjacent pair. There is no whole-index write-path rebuild.
//
// The discriminator in both tests below is `delta_segment_records`, the
// cumulative count of records appended to a delta segment. It separates the
// two paths an over-threshold insert can take: absorbed by a reopened delta
// (the counter keeps climbing) or spliced into the base graph inline (the
// counter stops). Asserting only that results stay correct would pass with the
// feature removed entirely, since the inline path is also correct.

/// Rows to insert past the delta limit. Large enough that the inline path is
/// unambiguously distinguishable from the absorbed one.
const THRESHOLD_OVERFLOW_ROWS: i64 = 40;
const THRESHOLD_DELTA_LIMIT: i64 = 10;

fn threshold_probe_setup(table: &str, index: &str) {
    Spi::run(&format!(
        "CREATE TABLE {table} (id bigint PRIMARY KEY, embedding vector(8) NOT NULL)"
    ))
    .expect("threshold probe table should be created");
    Spi::run(&format!(
        "INSERT INTO {table} SELECT n, {COMPACT_PROBE_VECTOR} FROM generate_series(1, 60) n"
    ))
    .expect("threshold probe base rows should insert");
    Spi::run(&format!(
        "CREATE INDEX {index} ON {table} \
         USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops)"
    ))
    .expect("threshold probe index should build");
    Spi::run(&format!(
        "SET pgcontext.hnsw_delta_segment_limit = {THRESHOLD_DELTA_LIMIT}"
    ))
    .expect("delta limit should apply");
    Spi::run("SET enable_seqscan = off").expect("seqscan off should apply");
}

fn threshold_probe_teardown() {
    Spi::run("RESET pgcontext.hnsw_delta_segment_limit").expect("delta limit should reset");
    Spi::run("RESET enable_seqscan").expect("seqscan should reset");
}

#[pg_test]
fn hnsw_bounded_rotation_keeps_every_insert_on_the_delta_path() {
    threshold_probe_setup("threshold_probe", "threshold_probe_hnsw");

    let appended_before = read_stat("delta_segment_records");
    Spi::run(&format!(
        "INSERT INTO threshold_probe SELECT n, {COMPACT_PROBE_VECTOR} \
           FROM generate_series(61, {}) n",
        60 + THRESHOLD_OVERFLOW_ROWS
    ))
    .expect("over-threshold rows should insert");
    let appended = read_stat("delta_segment_records") - appended_before;

    // Every row past the limit still reached a delta segment, which is only
    // possible if compaction drained and reopened it mid-insert.
    assert_eq!(
        appended, THRESHOLD_OVERFLOW_ROWS,
        "with bounded rotation, every insert must be absorbed by a \
         delta segment; {appended} of {THRESHOLD_OVERFLOW_ROWS} were"
    );
    let frozen: i64 = Spi::get_one(
        "SELECT frozen_mutation_records FROM pgcontext.hnsw_segment_stats('threshold_probe_hnsw')",
    )
    .expect("rotation stats should run")
    .expect("frozen count should not be null");
    assert_eq!(frozen, 0, "rotated live rows must not remain in exact mutation logs");

    let top_k = "SELECT id FROM threshold_probe \
         ORDER BY embedding OPERATOR(pgcontext.<=>) \
         (SELECT embedding FROM threshold_probe WHERE id = 75) \
         LIMIT 10";
    assert_eq!(
        compact_probe_ids(top_k),
        compact_probe_exact_ids(top_k),
        "compacting mid-insert must not change which rows the index returns"
    );

    threshold_probe_teardown();
}

#[pg_test]
fn hnsw_bounded_pair_compaction_rejects_over_budget_before_publication() {
    Spi::run(
        "CREATE TABLE bounded_budget_probe (
             id bigint PRIMARY KEY,
             embedding vector(384) NOT NULL
         )",
    )
    .expect("bounded budget table should be created");
    Spi::run(
        "INSERT INTO bounded_budget_probe
         SELECT n, array_fill(n::real, ARRAY[384])::vector
           FROM generate_series(1, 800) n",
    )
    .expect("bounded budget base rows should insert");
    Spi::run(
        "CREATE INDEX bounded_budget_probe_hnsw ON bounded_budget_probe
         USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops)",
    )
    .expect("bounded budget index should build");
    Spi::run("SET pgcontext.hnsw_delta_segment_limit = 400")
        .expect("wide delta limit should apply");
    Spi::run(
        "INSERT INTO bounded_budget_probe
         SELECT n, array_fill(n::real, ARRAY[384])::vector
           FROM generate_series(801, 1240) n",
    )
    .expect("bounded budget rows should rotate");
    let before: (i32, i64) = Spi::connect(|client| {
        let rows = client.select(
            "SELECT segment_count, directory_epoch
               FROM pgcontext.hnsw_segment_stats('bounded_budget_probe_hnsw')",
            Some(1),
            &[],
        )?;
        let row = rows.first();
        Ok::<_, spi::Error>((
            row.get::<i32>(1)?.expect("segment count should not be null"),
            row.get::<i64>(2)?.expect("directory epoch should not be null"),
        ))
    })
    .expect("preflight stats should decode");
    assert!(before.0 >= 2, "fixture must publish a pair");
    Spi::run("SET maintenance_work_mem = '1MB'").expect("small budget should apply");
    let rejected = PgTryBuilder::new(|| {
        Spi::run_with_args(
            "SELECT pgcontext._compact_hnsw_segment_pair(
                 'bounded_budget_probe_hnsw'::regclass, $1
             )",
            &[before.1.into()],
        )
        .expect("over-budget pair must not publish");
        false
    })
    .catch_when(PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE, |_| true)
    .execute();
    assert!(rejected, "pair compaction must enforce maintenance_work_mem");
    let after: (i32, i64) = Spi::connect(|client| {
        let rows = client.select(
            "SELECT segment_count, directory_epoch
               FROM pgcontext.hnsw_segment_stats('bounded_budget_probe_hnsw')",
            Some(1),
            &[],
        )?;
        let row = rows.first();
        Ok::<_, spi::Error>((
            row.get::<i32>(1)?.expect("segment count should not be null"),
            row.get::<i64>(2)?.expect("directory epoch should not be null"),
        ))
    })
    .expect("post-rejection stats should decode");
    assert_eq!(after, before, "budget rejection must precede publication");
    Spi::run("RESET maintenance_work_mem").expect("budget should reset");
    Spi::run("RESET pgcontext.hnsw_delta_segment_limit").expect("delta limit should reset");
}
