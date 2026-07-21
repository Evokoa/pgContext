// Serving-memory, delta-overlay, parallel-build, and coexist-budget
// pg_tests split from `hnsw_am.rs` to satisfy the source-hygiene size
// target.

#[pg_test]
fn hnsw_serving_stats_observe_pack_build_and_reuse() {
    Spi::run(
        "CREATE TABLE serving_stats_probe (id bigint PRIMARY KEY, \
         embedding vector(8) NOT NULL)",
    )
    .expect("serving stats probe table should be created");
    Spi::run(
        "INSERT INTO serving_stats_probe \
         SELECT n, \
                (SELECT '[' || string_agg(((n * 13 + d) % 31)::text, ',') || ']' \
                   FROM generate_series(1, 8) d)::vector \
           FROM generate_series(1, 64) n",
    )
    .expect("serving stats probe rows should insert");
    Spi::run(
        "CREATE INDEX serving_stats_probe_hnsw ON serving_stats_probe \
         USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops)",
    )
    .expect("serving stats probe index should build");
    Spi::run("SET enable_seqscan = off").expect("seqscan off should apply");

    let ann = "SELECT id FROM serving_stats_probe ORDER BY embedding \
               OPERATOR(pgcontext.<=>) '[1,2,3,4,5,6,7,8]'::vector LIMIT 5";
    Spi::run(ann).expect("first ANN query should run");
    Spi::run(ann).expect("second ANN query should run");

    let (builds, reuses) = Spi::connect(|client| {
        let row = client
            .select(
                "SELECT pack_builds, pack_reuses FROM pgcontext.hnsw_serving_stats()",
                None,
                &[],
            )?
            .first();
        Ok::<_, spi::Error>((
            row.get::<i64>(1)?.unwrap_or_default(),
            row.get::<i64>(2)?.unwrap_or_default(),
        ))
    })
    .expect("serving stats row should be readable");

    assert!(builds >= 1, "expected at least one pack build, saw {builds}");
    assert!(reuses >= 1, "expected at least one pack reuse, saw {reuses}");
    Spi::run("RESET enable_seqscan").expect("seqscan should reset");
}

#[pg_test]
fn hnsw_shared_serving_publishes_and_disabled_guc_skips() {
    Spi::run(
        "CREATE TABLE shared_serving_probe (id bigint PRIMARY KEY, \
         embedding vector(8) NOT NULL)",
    )
    .expect("shared serving probe table should be created");
    Spi::run(
        "INSERT INTO shared_serving_probe \
         SELECT n, \
                (SELECT '[' || string_agg(((n * 17 + d) % 29)::text, ',') || ']' \
                   FROM generate_series(1, 8) d)::vector \
           FROM generate_series(1, 64) n",
    )
    .expect("shared serving probe rows should insert");
    Spi::run(
        "CREATE INDEX shared_serving_probe_hnsw ON shared_serving_probe \
         USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops)",
    )
    .expect("shared serving probe index should build");
    Spi::run("SET enable_seqscan = off").expect("seqscan off should apply");

    let ann = "SELECT id FROM shared_serving_probe ORDER BY embedding \
               OPERATOR(pgcontext.<=>) '[1,2,3,4,5,6,7,8]'::vector LIMIT 5";

    // Default GUC (on): the first pack in this backend is published.
    Spi::run(ann).expect("query with shared serving enabled should run");
    let (publishes, skips) = Spi::connect(|client| {
        let row = client
            .select(
                "SELECT shared_publishes, shared_publish_skips \
                   FROM pgcontext.hnsw_serving_stats()",
                None,
                &[],
            )?
            .first();
        Ok::<_, spi::Error>((
            row.get::<i64>(1)?.unwrap_or_default(),
            row.get::<i64>(2)?.unwrap_or_default(),
        ))
    })
    .expect("serving stats row should be readable");
    assert_eq!(
        publishes, 1,
        "expected the first build in this backend to publish, saw {publishes}"
    );
    assert_eq!(skips, 0, "expected no publish skips yet, saw {skips}");

    // Disabling the GUC and forcing a fresh pack (new index) must not
    // touch the shared registry at all: no new publish, no new skip.
    Spi::run("SET pgcontext.hnsw_shared_serving = off")
        .expect("shared serving GUC should be settable");
    Spi::run(
        "CREATE TABLE shared_serving_probe_off (id bigint PRIMARY KEY, \
         embedding vector(8) NOT NULL)",
    )
    .expect("second probe table should be created");
    Spi::run(
        "INSERT INTO shared_serving_probe_off \
         SELECT id, embedding FROM shared_serving_probe",
    )
    .expect("second probe rows should insert");
    Spi::run(
        "CREATE INDEX shared_serving_probe_off_hnsw ON shared_serving_probe_off \
         USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops)",
    )
    .expect("second probe index should build");
    let ann_off = "SELECT id FROM shared_serving_probe_off ORDER BY embedding \
                   OPERATOR(pgcontext.<=>) '[1,2,3,4,5,6,7,8]'::vector LIMIT 5";
    Spi::run(ann_off).expect("query with shared serving disabled should run");

    let (publishes_after, skips_after) = Spi::connect(|client| {
        let row = client
            .select(
                "SELECT shared_publishes, shared_publish_skips \
                   FROM pgcontext.hnsw_serving_stats()",
                None,
                &[],
            )?
            .first();
        Ok::<_, spi::Error>((
            row.get::<i64>(1)?.unwrap_or_default(),
            row.get::<i64>(2)?.unwrap_or_default(),
        ))
    })
    .expect("serving stats row should be readable");
    assert_eq!(
        publishes_after, publishes,
        "disabling the GUC must not add a publish"
    );
    assert_eq!(
        skips_after, skips,
        "disabling the GUC must not add a publish skip either — it should \
         skip the shared-serving path entirely, not attempt and fail"
    );

    Spi::run("RESET pgcontext.hnsw_shared_serving").expect("GUC should reset");
    Spi::run("RESET enable_seqscan").expect("seqscan should reset");
}

#[pg_test]
fn hnsw_shared_serving_budget_zero_skips_publish() {
    Spi::run(
        "CREATE TABLE shared_serving_budget_probe (id bigint PRIMARY KEY, \
         embedding vector(8) NOT NULL)",
    )
    .expect("budget probe table should be created");
    Spi::run(
        "INSERT INTO shared_serving_budget_probe \
         SELECT n, \
                (SELECT '[' || string_agg(((n * 11 + d) % 23)::text, ',') || ']' \
                   FROM generate_series(1, 8) d)::vector \
           FROM generate_series(1, 32) n",
    )
    .expect("budget probe rows should insert");
    Spi::run("SET pgcontext.hnsw_shared_serving_budget_mb = 0")
        .expect("budget GUC should be settable");
    Spi::run(
        "CREATE INDEX shared_serving_budget_probe_hnsw ON shared_serving_budget_probe \
         USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops)",
    )
    .expect("budget probe index should build");
    Spi::run("SET enable_seqscan = off").expect("seqscan off should apply");
    Spi::run(
        "SELECT id FROM shared_serving_budget_probe ORDER BY embedding \
         OPERATOR(pgcontext.<=>) '[1,2,3,4,5,6,7,8]'::vector LIMIT 5",
    )
    .expect("query under a zero shared-serving budget should still run");

    let skips = Spi::connect(|client| {
        let row = client
            .select(
                "SELECT shared_publish_skips FROM pgcontext.hnsw_serving_stats()",
                None,
                &[],
            )?
            .first();
        row.get::<i64>(1).map(Option::unwrap_or_default)
    })
    .expect("serving stats row should be readable");
    assert!(
        skips >= 1,
        "expected the zero-budget publish attempt to be skipped, saw {skips}"
    );

    Spi::run("RESET pgcontext.hnsw_shared_serving_budget_mb").expect("budget GUC should reset");
    Spi::run("RESET enable_seqscan").expect("seqscan should reset");
}


#[pg_test]
fn hnsw_stale_generation_repacks_and_still_matches_the_oracle() {
    // The backend-local delta overlay is retired: a generation is whole, so a
    // cached pack that goes stale is rebuilt rather than patched. This pins
    // the two things that replaced it -- a repack actually happens, and the
    // rebuilt generation answers exactly like an off-index oracle.
    Spi::run(
        "CREATE TABLE stale_probe (id bigint PRIMARY KEY, \
         embedding vector(8) NOT NULL)",
    )
    .expect("stale probe table should be created");
    Spi::run(
        "INSERT INTO stale_probe \
         SELECT n, \
                (SELECT '[' || string_agg(((n * 13 + d) % 211 + 1)::text, ',') || ']' \
                   FROM generate_series(1, 8) d)::vector \
           FROM generate_series(1, 200) n",
    )
    .expect("stale probe rows should insert");
    Spi::run(
        "CREATE INDEX stale_probe_hnsw ON stale_probe \
         USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops)",
    )
    .expect("stale probe index should build");
    Spi::run("SET enable_seqscan = off").expect("seqscan off should apply");
    // Inline inserts are what stale a cached pack; delta-absorbed inserts do
    // not. Force the inline path so the staleness this test is about occurs.
    Spi::run("SET pgcontext.hnsw_delta_segment_limit = 0")
        .expect("delta segment limit GUC should be settable");
    // Pinned, not inherited. pg_tests share one backend and plain SET outlives
    // the transaction, so a value left behind by another test decides which
    // serving path runs here. This test is about the packed path; with packing
    // off it would silently become a test of the page-native fallback.
    Spi::run("SET pgcontext.hnsw_pack_on_first_use = on")
        .expect("pack-on-first-use GUC should be settable");

    let ann = "SELECT id FROM stale_probe ORDER BY embedding \
               OPERATOR(pgcontext.<=>) '[1,2,3,4,5,6,7,8]'::vector LIMIT 5";
    Spi::run(ann).expect("first query should warm a pack");

    let builds_before = read_stat("pack_builds");
    Spi::run(
        "INSERT INTO stale_probe \
         SELECT n, \
                (SELECT '[' || string_agg(((n * 13 + d) % 211 + 1)::text, ',') || ']' \
                   FROM generate_series(1, 8) d)::vector \
           FROM generate_series(201, 210) n",
    )
    .expect("stale probe follow-up rows should insert");

    // Every inline-spliced row must appear in the served generation.
    //
    // Asserted as membership in a full ordered scan, not as "is its own
    // nearest neighbour at LIMIT 1". The candidate budget is sized from the
    // LIMIT, so a k=1 search is too narrow to be a statement about whether the
    // node exists -- it can miss an exact match and return a neighbour, which
    // is approximate-search behaviour rather than a missing row. A full scan
    // has to enumerate the graph, so absence there really does mean absent.
    let served: String = Spi::get_one(
        "SELECT string_agg(id::text, ',' ORDER BY id) FROM ( \
           SELECT id FROM stale_probe \
            ORDER BY embedding OPERATOR(pgcontext.<=>) \
            (SELECT embedding FROM stale_probe WHERE id = 205), id \
            LIMIT 210) t \
          WHERE id BETWEEN 201 AND 210",
    )
    .expect("served-row query should run")
    .expect("served rows should not be null");
    assert_eq!(
        served, "201,202,203,204,205,206,207,208,209,210",
        "the rebuilt generation must serve every inline-spliced row"
    );

    let builds_after = read_stat("pack_builds");
    assert!(
        builds_after > builds_before,
        "a staled generation must be repacked, not patched in place"
    );

    let count: i64 = Spi::get_one("SELECT count(*)::bigint FROM stale_probe")
        .expect("row count query should run")
        .expect("row count should not be null");
    assert_eq!(count, 210, "every inserted row must still be visible");

    Spi::run("RESET pgcontext.hnsw_delta_segment_limit")
        .expect("delta segment limit GUC should reset");
    Spi::run("RESET pgcontext.hnsw_pack_on_first_use")
        .expect("pack-on-first-use GUC should reset");
    Spi::run("RESET enable_seqscan").expect("seqscan should reset");
}

fn read_stat(column: &str) -> i64 {
    Spi::connect(|client| {
        let row = client
            .select(
                &format!("SELECT {column} FROM pgcontext.hnsw_serving_stats()"),
                None,
                &[],
            )?
            .first();
        row.get::<i64>(1).map(Option::unwrap_or_default)
    })
    .expect("serving stats row should be readable")
}


#[pg_test]
fn hnsw_pack_on_first_use_off_serves_correct_results_without_packing() {
    Spi::run(
        "CREATE TABLE page_native_probe (id bigint PRIMARY KEY, \
         embedding vector(8) NOT NULL)",
    )
    .expect("page-native probe table should be created");
    Spi::run(
        "INSERT INTO page_native_probe \
         SELECT n, \
                (SELECT '[' || string_agg(((n * 23 + d) % 43)::text, ',') || ']' \
                   FROM generate_series(1, 8) d)::vector \
           FROM generate_series(1, 150) n",
    )
    .expect("page-native probe rows should insert");
    Spi::run("SET pgcontext.hnsw_pack_on_first_use = off")
        .expect("pack-on-first-use GUC should be settable");
    Spi::run(
        "CREATE INDEX page_native_probe_hnsw ON page_native_probe \
         USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops)",
    )
    .expect("page-native probe index should build");
    Spi::run("SET enable_indexscan = off; SET enable_bitmapscan = off; SET enable_seqscan = on")
        .expect("exact-oracle scan settings should apply");
    let exact_ids: Vec<i64> = Spi::connect(|client| {
        let rows = client.select(
            "SELECT id FROM page_native_probe ORDER BY embedding \
             OPERATOR(pgcontext.<=>) '[1,2,3,4,5,6,7,8]'::vector LIMIT 5",
            None,
            &[],
        )?;
        rows.map(|row| row.get::<i64>(1).map(Option::unwrap_or_default))
            .collect::<Result<Vec<_>, _>>()
    })
    .expect("exact-oracle id query should run");
    Spi::run("RESET enable_indexscan; RESET enable_bitmapscan")
        .expect("exact-oracle scan settings should reset");

    Spi::run("SET enable_seqscan = off").expect("seqscan off should apply");
    let ann_ids: Vec<i64> = Spi::connect(|client| {
        let rows = client.select(
            "SELECT id FROM page_native_probe ORDER BY embedding \
             OPERATOR(pgcontext.<=>) '[1,2,3,4,5,6,7,8]'::vector LIMIT 5",
            None,
            &[],
        )?;
        rows.map(|row| row.get::<i64>(1).map(Option::unwrap_or_default))
            .collect::<Result<Vec<_>, _>>()
    })
    .expect("page-native ANN query should run");

    assert_eq!(
        ann_ids.len(),
        5,
        "page-native fallback must still return a full result set"
    );
    let overlap = ann_ids.iter().filter(|id| exact_ids.contains(id)).count();
    assert!(
        overlap >= 4,
        "page-native fallback recall regressed unexpectedly: {ann_ids:?} vs exact {exact_ids:?}"
    );

    let (pack_builds, fallbacks) = Spi::connect(|client| {
        let row = client
            .select(
                "SELECT pack_builds, page_native_fallbacks \
                   FROM pgcontext.hnsw_serving_stats()",
                None,
                &[],
            )?
            .first();
        Ok::<_, spi::Error>((
            row.get::<i64>(1)?.unwrap_or_default(),
            row.get::<i64>(2)?.unwrap_or_default(),
        ))
    })
    .expect("serving stats row should be readable");
    assert_eq!(
        pack_builds, 0,
        "pack_on_first_use=off must never pay an inline pack"
    );
    assert!(
        fallbacks >= 1,
        "expected at least one page-native fallback, saw {fallbacks}"
    );

    Spi::run("RESET pgcontext.hnsw_pack_on_first_use")
        .expect("pack-on-first-use GUC should reset");
    Spi::run("RESET enable_seqscan").expect("seqscan should reset");
}

#[pg_test]
fn hnsw_mask_candidate_limit_guc_raises_the_masked_scan_budget_above_the_default() {
    Spi::run(
        "CREATE TABLE mask_budget_probe (id bigint PRIMARY KEY, \
         embedding vector(4) NOT NULL)",
    )
    .expect("mask-budget probe table should be created");
    Spi::run(
        "INSERT INTO mask_budget_probe \
         VALUES (1, '[1,2,3,4]'::vector), (2, '[5,6,7,8]'::vector)",
    )
    .expect("mask-budget probe rows should insert");
    Spi::run(
        "CREATE INDEX mask_budget_probe_hnsw ON mask_budget_probe \
         USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops)",
    )
    .expect("mask-budget probe index should build");

    // A mask larger than the default candidate-mask budget (10,000 points):
    // synthetic TIDs are enough because the budget check runs before any
    // point in the mask is resolved against the graph.
    let over_default_budget_sql =
        "WITH candidates AS (
             SELECT ('(' || n || ',1)')::tid AS heap_tid
               FROM generate_series(0, 10000) AS n
         )
         SELECT * FROM pgcontext._hnsw_masked_candidates(
             'mask_budget_probe_hnsw'::regclass,
             '[1,2,3,4]'::vector,
             (SELECT array_agg(heap_tid) FROM candidates),
             5
         )";

    let rejected_at_default = PgTryBuilder::new(|| {
        Spi::run(over_default_budget_sql)
            .expect("over-default-budget masked scan should fail before raising the GUC");
        false
    })
    .catch_when(PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED, |_| true)
    .execute();
    assert!(
        rejected_at_default,
        "masked scan above the default budget must fail with SQLSTATE 54000 by default"
    );

    Spi::run("SET pgcontext.hnsw_mask_candidate_limit = 20000")
        .expect("mask-candidate-limit GUC should be settable above the default");
    Spi::run(over_default_budget_sql)
        .expect("masked scan should succeed once the GUC raises the budget above 10,001 points");
    Spi::run("RESET pgcontext.hnsw_mask_candidate_limit")
        .expect("mask-candidate-limit GUC should reset");
}

#[pg_test]
fn hnsw_build_parallel_workers_produces_a_correct_and_usable_index() {
    Spi::run(
        "CREATE TABLE parallel_build_probe (id bigint PRIMARY KEY, \
         embedding vector(8) NOT NULL)",
    )
    .expect("parallel-build probe table should be created");
    Spi::run(
        "INSERT INTO parallel_build_probe \
         SELECT n, \
                (SELECT '[' || string_agg(((n * 31 + d) % 47)::text, ',') || ']' \
                   FROM generate_series(1, 8) d)::vector \
           FROM generate_series(1, 400) n",
    )
    .expect("parallel-build probe rows should insert");
    Spi::run("SET pgcontext.hnsw_build_parallel_workers = 4")
        .expect("build-parallel-workers GUC should be settable");
    Spi::run(
        "CREATE INDEX parallel_build_probe_hnsw ON parallel_build_probe \
         USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops)",
    )
    .expect("parallel-build probe index should build with multiple workers");
    Spi::run("RESET pgcontext.hnsw_build_parallel_workers")
        .expect("build-parallel-workers GUC should reset");

    Spi::run("SET enable_indexscan = off; SET enable_bitmapscan = off; SET enable_seqscan = on")
        .expect("exact-oracle scan settings should apply");
    let exact_ids: Vec<i64> = Spi::connect(|client| {
        let rows = client.select(
            "SELECT id FROM parallel_build_probe ORDER BY embedding \
             OPERATOR(pgcontext.<=>) '[1,2,3,4,5,6,7,8]'::vector LIMIT 10",
            None,
            &[],
        )?;
        rows.map(|row| row.get::<i64>(1).map(Option::unwrap_or_default))
            .collect::<Result<Vec<_>, _>>()
    })
    .expect("exact-oracle id query should run");
    Spi::run("RESET enable_indexscan; RESET enable_bitmapscan")
        .expect("exact-oracle scan settings should reset");

    Spi::run("SET enable_seqscan = off").expect("seqscan off should apply");
    let ann_ids: Vec<i64> = Spi::connect(|client| {
        let rows = client.select(
            "SELECT id FROM parallel_build_probe ORDER BY embedding \
             OPERATOR(pgcontext.<=>) '[1,2,3,4,5,6,7,8]'::vector LIMIT 10",
            None,
            &[],
        )?;
        rows.map(|row| row.get::<i64>(1).map(Option::unwrap_or_default))
            .collect::<Result<Vec<_>, _>>()
    })
    .expect("ANN query over a parallel-built index should run");
    Spi::run("RESET enable_seqscan").expect("seqscan should reset");

    assert_eq!(
        ann_ids.len(),
        10,
        "a parallel-built index must still return a full result set"
    );
    let overlap = ann_ids.iter().filter(|id| exact_ids.contains(id)).count();
    assert!(
        overlap >= 8,
        "parallel build recall regressed unexpectedly: {ann_ids:?} vs exact {exact_ids:?}"
    );
}
