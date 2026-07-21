// Segmented-write delta-region pg_tests (P2-S3): inserts absorbed by the
// bounded delta region instead of a full graph splice, and the fallback to
// the legacy inline path once the region is full.
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
            let result = client.select(sql, None, &[]).expect("top-k query should run");
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
fn hnsw_delta_segment_falls_back_to_inline_insert_beyond_the_limit() {
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

    // 10 inserts against a limit of 3: the first 3 land in the delta
    // region, the rest fall back to the legacy inline graph-splice path.
    // Every row must still be visible and correctly ordered either way.
    Spi::run(&format!(
        "INSERT INTO delta_segment_limit_probe \
         SELECT n, {DELTA_PROBE_VECTOR} \
           FROM generate_series(51, 60) n"
    ))
    .expect("delta segment limit probe follow-up rows should insert");

    let count: i64 = Spi::get_one("SELECT count(*)::bigint FROM delta_segment_limit_probe")
        .expect("row count query should run")
        .expect("row count should not be null");
    assert_eq!(count, 60, "every inserted row must be visible regardless of insert path");

    // Row 55 is past the delta limit, so it reached the graph through the
    // inline fallback while 51-53 sit in the delta region: the top-k has to
    // span both paths.
    let top_k = "SELECT id FROM delta_segment_limit_probe \
         ORDER BY embedding OPERATOR(pgcontext.<=>) \
         (SELECT embedding FROM delta_segment_limit_probe WHERE id = 55) \
         LIMIT 10";

    let read_ids = |sql: &str| -> Vec<i64> {
        Spi::connect(|client| {
            let result = client.select(sql, None, &[]).expect("top-k query should run");
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
        "results must match the exact oracle whether a row landed in the delta region or the inline path"
    );

    Spi::run("RESET pgcontext.hnsw_delta_segment_limit")
        .expect("delta segment limit GUC should reset");
    Spi::run("RESET enable_seqscan").expect("seqscan should reset");
}
