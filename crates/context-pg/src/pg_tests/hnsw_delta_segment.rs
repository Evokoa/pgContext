// Segmented-write delta-region pg_tests (P2-S3): inserts absorbed by the
// bounded delta region instead of a full graph splice, and immutable segment
// rotation once the active region is full.
//
// VACUUM tombstoning of delta-only rows is covered by
// `scripts/check-hnsw-vacuum.sh` instead: `#[pg_test]` bodies run inside a
// transaction, and VACUUM cannot run in one.

/// Per-row probe vector: dimension `i` is `(n * primes[i]) % 211 + 1`.
///
/// A distinct prime per dimension keeps directions spread out under cosine,
/// and a modulus above the row count keeps every row's vector unique. A
/// single multiplier with a small modulus does neither — `(n * 13 + d) % 37`
/// gives rows 16 and 201 (and 53, 90, 127, 164) byte-identical vectors, so a
/// nearest-neighbour assertion on any of them is really asserting an
/// arbitrary tie-break. `ORDER BY ord` is required: `string_agg` has no
/// implicit input order, so without it the dimension order is unspecified.
const DELTA_PROBE_VECTOR: &str = "(SELECT '[' || string_agg(((n * p) % 211 + 1)::text, ',' \
     ORDER BY ord) || ']' \
     FROM unnest(ARRAY[13,29,41,53,67,79,89,101]) WITH ORDINALITY AS primes(p, ord))::vector";

#[pg_test]
fn hnsw_delta_segment_serves_inserted_rows_without_a_repack() {
    Spi::run(
        "CREATE TABLE delta_segment_probe (id bigint PRIMARY KEY, \
         embedding vector(8) NOT NULL)",
    )
    .expect("delta segment probe table should be created");
    Spi::run(&format!(
        "INSERT INTO delta_segment_probe \
         SELECT n, {DELTA_PROBE_VECTOR} \
           FROM generate_series(1, 200) n"
    ))
    .expect("delta segment probe base rows should insert");
    Spi::run(
        "CREATE INDEX delta_segment_probe_hnsw ON delta_segment_probe \
         USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops)",
    )
    .expect("delta segment probe index should build");
    Spi::run("SET enable_seqscan = off").expect("seqscan off should apply");

    let records_before = read_stat("delta_segment_records");
    let scans_before = read_stat("delta_segment_scans");

    // Every row inserted after CREATE INDEX must land in the delta region,
    // not a graph splice: no REINDEX or repack happens between insert and
    // query below.
    Spi::run(&format!(
        "INSERT INTO delta_segment_probe \
         SELECT n, {DELTA_PROBE_VECTOR} \
           FROM generate_series(201, 205) n"
    ))
    .expect("delta segment probe follow-up rows should insert");

    let records_after_insert = read_stat("delta_segment_records");
    assert_eq!(
        records_after_insert - records_before,
        5,
        "each inserted row should append exactly one delta record"
    );

    // Row 201 lives only in the delta region and, with the probe fixture, is
    // its own unique nearest neighbour by a wide margin (the next row is
    // ~0.03 cosine away), so a correct merge puts it first and matches the
    // exact ordering for the rest of the top-k.
    let top_k = "SELECT id FROM delta_segment_probe \
         ORDER BY embedding OPERATOR(pgcontext.<=>) \
         (SELECT embedding FROM delta_segment_probe WHERE id = 201) \
         LIMIT 5";

    let read_ids = |sql: &str| -> Vec<i64> {
        Spi::connect(|client| {
            let result = client
                .select(sql, None, &[])
                .expect("top-k query should run");
            let mut ids = Vec::new();
            for row in result {
                ids.push(row.get::<i64>(1).unwrap().unwrap_or_default());
            }
            Ok::<_, spi::Error>(ids)
        })
        .expect("top-k rows should decode")
    };

    // A real oracle has to be forced off the index; running the same plan
    // twice compares the HNSW path against itself and passes even when the
    // delta region is never merged at all.
    Spi::run("SET enable_indexscan = off").expect("indexscan off should apply");
    Spi::run("SET enable_seqscan = on").expect("seqscan on should apply");
    let exact = read_ids(top_k);
    Spi::run("SET enable_indexscan = on").expect("indexscan on should apply");
    Spi::run("SET enable_seqscan = off").expect("seqscan off should apply");
    let ann = read_ids(top_k);

    assert_eq!(
        exact.first().copied(),
        Some(201),
        "fixture check: the exact oracle must rank the delta-only row first"
    );
    assert_eq!(
        ann, exact,
        "delta-region rows must merge into the top-k exactly like base-graph rows"
    );

    let scans_after = read_stat("delta_segment_scans");
    assert!(
        scans_after > scans_before,
        "queries after a delta append must merge the delta region, saw {scans_before} -> {scans_after}"
    );

    Spi::run("RESET enable_seqscan").expect("seqscan should reset");
}

#[pg_test]
fn hnsw_delta_segment_rotates_beyond_the_limit() {
    Spi::run(
        "CREATE TABLE delta_segment_limit_probe (id bigint PRIMARY KEY, \
         embedding vector(8) NOT NULL)",
    )
    .expect("delta segment limit probe table should be created");
    Spi::run(&format!(
        "INSERT INTO delta_segment_limit_probe \
         SELECT n, {DELTA_PROBE_VECTOR} \
           FROM generate_series(1, 50) n"
    ))
    .expect("delta segment limit probe base rows should insert");
    Spi::run(
        "CREATE INDEX delta_segment_limit_probe_hnsw ON delta_segment_limit_probe \
         USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops)",
    )
    .expect("delta segment limit probe index should build");
    Spi::run("SET enable_seqscan = off").expect("seqscan off should apply");
    Spi::run("SET pgcontext.hnsw_delta_segment_limit = 3")
        .expect("delta segment limit GUC should be settable");
    Spi::run("SET pgcontext.hnsw_segment_parallel_workers = 2")
        .expect("segment worker limit should be settable");
    Spi::run("SET pgcontext.hnsw_mmap_serving = false")
        .expect("test should force owned local packs");
    Spi::run("SET pgcontext.hnsw_shared_serving = false")
        .expect("test should force owned local packs");
    let rotations_before = read_stat("segment_rotations");
    let multi_scans_before = read_stat("multi_segment_scans");
    let parallel_scans_before = read_stat("parallel_segment_scans");
    let parallel_denials_before = read_stat("parallel_admission_denials");
    let serial_degradations_before = read_stat("serial_segment_degradations");

    // Ten inserts against a limit of three produce three bounded rotations
    // and leave one row in the active exact delta.
    Spi::run(&format!(
        "INSERT INTO delta_segment_limit_probe \
         SELECT n, {DELTA_PROBE_VECTOR} \
           FROM generate_series(51, 60) n"
    ))
    .expect("delta segment limit probe follow-up rows should insert");

    let count: i64 = Spi::get_one("SELECT count(*)::bigint FROM delta_segment_limit_probe")
        .expect("row count query should run")
        .expect("row count should not be null");
    assert_eq!(
        count, 60,
        "every inserted row must be visible regardless of insert path"
    );
    let rotations_after = read_stat("segment_rotations");
    assert_eq!(
        rotations_after - rotations_before,
        3,
        "every full delta should rotate exactly once"
    );

    // Row 55 belongs to the second immutable segment while row 60 remains in
    // the active delta: the top-k has to span both storage paths.
    let top_k = "SELECT id FROM delta_segment_limit_probe \
         ORDER BY embedding OPERATOR(pgcontext.<=>) \
         (SELECT embedding FROM delta_segment_limit_probe WHERE id = 55) \
         LIMIT 10";

    let read_ids = |sql: &str| -> Vec<i64> {
        Spi::connect(|client| {
            let result = client
                .select(sql, None, &[])
                .expect("top-k query should run");
            let mut ids = Vec::new();
            for row in result {
                ids.push(row.get::<i64>(1).unwrap().unwrap_or_default());
            }
            Ok::<_, spi::Error>(ids)
        })
        .expect("top-k rows should decode")
    };

    // Forced off the index, so this is a real oracle rather than the same
    // HNSW plan compared against itself.
    Spi::run("SET enable_indexscan = off").expect("indexscan off should apply");
    Spi::run("SET enable_seqscan = on").expect("seqscan on should apply");
    let exact = read_ids(top_k);
    Spi::run("SET enable_indexscan = on").expect("indexscan on should apply");
    Spi::run("SET enable_seqscan = off").expect("seqscan off should apply");
    let ann = read_ids(top_k);

    assert_eq!(
        exact.first().copied(),
        Some(55),
        "fixture check: the exact oracle must rank the probe row first"
    );
    assert_eq!(
        ann, exact,
        "results must match the exact oracle across immutable segments and the active delta"
    );
    let multi_scans_after = read_stat("multi_segment_scans");
    assert!(
        multi_scans_after > multi_scans_before,
        "the index query should report multi-segment fan-out"
    );
    let parallel_scans_after = read_stat("parallel_segment_scans");
    let parallel_denials_after = read_stat("parallel_admission_denials");
    let serial_degradations_after = read_stat("serial_segment_degradations");
    assert!(
        parallel_scans_after > parallel_scans_before,
        "owned immutable packs should be admitted to bounded parallel search; \
         admission denials={}, serial degradations={}",
        parallel_denials_after - parallel_denials_before,
        serial_degradations_after - serial_degradations_before,
    );
    let frozen_live_history: i64 = Spi::get_one(
        "SELECT frozen_mutation_records
           FROM pgcontext.hnsw_segment_stats('delta_segment_limit_probe_hnsw')",
    )
    .expect("segment residual stats should run")
    .expect("frozen mutation count should not be null");
    assert_eq!(
        frozen_live_history, 0,
        "rotated live rows belong only to ANN segments, never an exact-scanned frozen log"
    );

    Spi::run("RESET pgcontext.hnsw_delta_segment_limit")
        .expect("delta segment limit GUC should reset");
    Spi::run("RESET pgcontext.hnsw_segment_parallel_workers")
        .expect("segment worker limit should reset");
    Spi::run("RESET pgcontext.hnsw_mmap_serving").expect("mapped serving should reset");
    Spi::run("RESET pgcontext.hnsw_shared_serving").expect("shared serving should reset");
    Spi::run("RESET enable_seqscan").expect("seqscan should reset");
}

#[pg_test]
fn supervised_worker_compacts_one_bounded_hnsw_pair() {
    Spi::run(
        "CREATE TABLE supervised_segment_probe (
             id bigint PRIMARY KEY,
             embedding vector(8) NOT NULL
         )",
    )
    .expect("supervised segment table should be created");
    Spi::run(&format!(
        "INSERT INTO supervised_segment_probe
         SELECT n, {DELTA_PROBE_VECTOR}
           FROM generate_series(1, 20) n"
    ))
    .expect("supervised segment base rows should insert");
    Spi::run(
        "SELECT pgcontext.create_collection(
             'supervised_segment_probe', 'public.supervised_segment_probe'
         )",
    )
    .expect("supervised segment collection should be created");
    Spi::run(
        "CREATE INDEX supervised_segment_probe_hnsw ON supervised_segment_probe
         USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops)",
    )
    .expect("supervised segment index should build");
    Spi::run("SET pgcontext.hnsw_delta_segment_limit = 1").expect("delta limit should be settable");
    Spi::run("SET pgcontext.build_workers_enabled = false")
        .expect("test should use the deterministic manual worker seam");
    Spi::run(&format!(
        "INSERT INTO supervised_segment_probe
         SELECT n, {DELTA_PROBE_VECTOR}
           FROM generate_series(21, 24) n"
    ))
    .expect("follow-up rows should create immutable segments");

    let (before, active_blocks): (i32, i64) = Spi::connect(|client| {
        let rows = client.select(
            "SELECT segment_count, active_delta_blocks
           FROM pgcontext.hnsw_segment_stats('supervised_segment_probe_hnsw')",
            Some(1),
            &[],
        )?;
        let row = rows.first();
        Ok::<_, spi::Error>((
            row.get::<i32>(1)?
                .expect("segment count should not be null"),
            row.get::<i64>(2)?
                .expect("active block count should not be null"),
        ))
    })
    .expect("segment stats should run");
    assert!(
        before >= 4,
        "fixture must publish multiple immutable segments"
    );

    let job_id: i64 = Spi::get_one(
        "SELECT build_job_id
           FROM pgcontext.enqueue_hnsw_compaction(
               'supervised_segment_probe', 'supervised_segment_probe_hnsw'
           )",
    )
    .expect("bounded compaction should enqueue")
    .expect("enqueued job id should not be null");
    for _ in 0..4 {
        assert!(
            crate::build_worker::process_one_step(),
            "each bounded lifecycle step should progress"
        );
    }

    let after: i32 = Spi::get_one(
        "SELECT segment_count
           FROM pgcontext.hnsw_segment_stats('supervised_segment_probe_hnsw')",
    )
    .expect("post-compaction stats should run")
    .expect("post-compaction segment count should not be null");
    assert_eq!(after, before - 1, "one worker job must compact one pair");
    let active_blocks_after: i64 = Spi::get_one(
        "SELECT active_delta_blocks
           FROM pgcontext.hnsw_segment_stats('supervised_segment_probe_hnsw')",
    )
    .expect("post-compaction active extent stats should run")
    .expect("active extent should not be null");
    assert_eq!(
        active_blocks_after, active_blocks,
        "pair compaction pages must not expand the exact active-delta extent"
    );
    let status: String = Spi::get_one_with_args(
        "SELECT status FROM pgcontext._build_jobs WHERE build_job_id = $1",
        &[job_id.into()],
    )
    .expect("worker status should be readable")
    .expect("worker status should not be null");
    assert_eq!(status, "completed");
    let expected_epoch: i64 = Spi::get_one_with_args(
        "SELECT config_revision FROM pgcontext._build_jobs WHERE build_job_id = $1",
        &[job_id.into()],
    )
    .expect("captured compaction epoch should be readable")
    .expect("captured compaction epoch should not be null");
    let retry_applied: bool = Spi::get_one_with_args(
        "SELECT pgcontext._compact_hnsw_segment_pair(
             'supervised_segment_probe_hnsw'::regclass, $1
         )",
        &[expected_epoch.into()],
    )
    .expect("same-intent retry should be accepted")
    .expect("same-intent retry result should not be null");
    assert!(
        !retry_applied,
        "a published job intent must be a retry no-op"
    );
    let after_retry: i32 = Spi::get_one(
        "SELECT segment_count
           FROM pgcontext.hnsw_segment_stats('supervised_segment_probe_hnsw')",
    )
    .expect("post-retry stats should run")
    .expect("post-retry count should not be null");
    assert_eq!(after_retry, after, "retry must not compact a second pair");

    Spi::run("RESET pgcontext.hnsw_delta_segment_limit").expect("delta limit should reset");
    Spi::run("RESET pgcontext.build_workers_enabled").expect("worker setting should reset");
}

#[pg_test]
fn hnsw_segment_saturation_keeps_mutation_extents_disjoint() {
    Spi::run(
        "CREATE TABLE segment_saturation_probe (
             id bigint PRIMARY KEY,
             embedding vector(2) NOT NULL
         );
         INSERT INTO segment_saturation_probe VALUES (1, '[1,1]');
         CREATE INDEX segment_saturation_probe_hnsw ON segment_saturation_probe
         USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops);
         SET pgcontext.hnsw_delta_segment_limit = 1",
    )
    .expect("saturation fixture should initialize");
    for id in 2..=35 {
        Spi::run(&format!(
            "INSERT INTO segment_saturation_probe VALUES ({id}, '[{id},1]')"
        ))
        .expect("each insert should rotate or compact bounded segments");
    }
    let stats = Spi::connect(|client| {
        let rows = client.select(
            "SELECT segment_count, active_delta_records, immutable_rows
               FROM pgcontext.hnsw_segment_stats('segment_saturation_probe_hnsw')",
            Some(1),
            &[],
        )?;
        let row = rows.first();
        Ok::<_, spi::Error>((
            row.get::<i32>(1)?
                .expect("segment_count should not be null"),
            row.get::<i64>(2)?
                .expect("active_delta_records should not be null"),
            row.get::<i64>(3)?
                .expect("immutable_rows should not be null"),
        ))
    })
    .expect("saturation stats should decode");
    assert_eq!(stats, (16, 1, 34));

    Spi::run("SET LOCAL enable_seqscan = off").expect("index scan should be forced");
    let nearest: i64 = Spi::get_one(
        "SELECT id FROM segment_saturation_probe
         ORDER BY embedding OPERATOR(pgcontext.<=>) '[30,1]'::vector
         LIMIT 1",
    )
    .expect("saturated index query should run")
    .expect("nearest row should not be null");
    assert_eq!(nearest, 30);
    Spi::run("RESET pgcontext.hnsw_delta_segment_limit").expect("delta limit should reset");
}
