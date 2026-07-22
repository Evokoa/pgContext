#[pg_test]
fn hnsw_access_method_is_registered() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT am.amname::text, am.amtype::text
                   FROM pg_catalog.pg_am am
                  WHERE am.amname = 'pgcontext_hnsw'",
                None,
                &[],
            )
            .expect("HNSW access method lookup failed");

        let row = result.first();
        Ok::<_, spi::Error>((
            row.get::<String>(1)?.unwrap_or_default(),
            row.get::<String>(2)?.unwrap_or_default(),
        ))
    })
    .expect("HNSW access method row failed");

    assert_eq!(rows, ("pgcontext_hnsw".to_owned(), "i".to_owned()));
}

#[pg_test]
fn hnsw_dense_metric_opclasses_bind_exact_operators_and_support_functions() {
    let bindings = Spi::connect(|client| {
        let result = client.select(
            "SELECT opc.opcname::text, opr.oprname::text, proc.proname::text
               FROM pg_catalog.pg_opclass opc
               JOIN pg_catalog.pg_am am ON am.oid = opc.opcmethod
               JOIN pg_catalog.pg_namespace nsp ON nsp.oid = opc.opcnamespace
               JOIN pg_catalog.pg_amop amop
                 ON amop.amopfamily = opc.opcfamily
                AND amop.amoplefttype = opc.opcintype
                AND amop.amoprighttype = opc.opcintype
                AND amop.amopstrategy = 1
               JOIN pg_catalog.pg_operator opr ON opr.oid = amop.amopopr
               JOIN pg_catalog.pg_amproc amproc
                 ON amproc.amprocfamily = opc.opcfamily
                AND amproc.amproclefttype = opc.opcintype
                AND amproc.amprocrighttype = opc.opcintype
                AND amproc.amprocnum = 1
               JOIN pg_catalog.pg_proc proc ON proc.oid = amproc.amproc
              WHERE nsp.nspname = 'pgcontext'
                AND am.amname = 'pgcontext_hnsw'
                AND opc.opcintype = 'vector'::regtype
              ORDER BY opc.opcname",
            None,
            &[],
        )?;
        let mut rows = Vec::new();
        for row in result {
            rows.push((
                row.get::<String>(1)?.unwrap_or_default(),
                row.get::<String>(2)?.unwrap_or_default(),
                row.get::<String>(3)?.unwrap_or_default(),
            ));
        }
        Ok::<_, spi::Error>(rows)
    })
    .expect("dense HNSW opclass catalog query should succeed");

    assert_eq!(
        bindings,
        vec![
            (
                "vector_hnsw_cosine_ops".to_owned(),
                "<=>".to_owned(),
                "cosine_distance".to_owned(),
            ),
            (
                "vector_hnsw_ip_ops".to_owned(),
                "<#>".to_owned(),
                "negative_inner_product".to_owned(),
            ),
            (
                "vector_hnsw_l1_ops".to_owned(),
                "<+>".to_owned(),
                "l1_distance".to_owned(),
            ),
            (
                "vector_hnsw_ops".to_owned(),
                "<->".to_owned(),
                "hnsw_l2_distance".to_owned(),
            ),
        ]
    );
}

#[pg_test]
fn hnsw_access_method_orderby_operator_returns_float8_distance() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT pg_typeof('[1,2,3]'::vector OPERATOR(pgcontext.<->) '[4,6,3]'::vector)::text,
                        '[1,2,3]'::vector OPERATOR(pgcontext.<->) '[4,6,3]'::vector",
                None,
                &[],
            )
            .expect("HNSW order-by operator type query should succeed");

        let row = result.first();
        Ok::<_, spi::Error>((
            row.get::<String>(1)?.unwrap_or_default(),
            row.get::<f64>(2)?.unwrap_or_default(),
        ))
    })
    .expect("HNSW order-by operator type row should decode");

    assert_eq!(rows.0, "double precision");
    assert_eq!(rows.1, 5.0);
}

#[pg_test]
fn hnsw_metric_build_and_persisted_scan_cover_all_dense_opclasses() {
    let cases = [
        ("l2", "vector_hnsw_ops", "<->", vec![1, 2, 3, 4]),
        ("ip", "vector_hnsw_ip_ops", "<#>", vec![3, 4, 1, 2]),
        (
            "cosine",
            "vector_hnsw_cosine_ops",
            "<=>",
            vec![3, 4, 1, 2],
        ),
        ("l1", "vector_hnsw_l1_ops", "<+>", vec![1, 2, 3, 4]),
    ];

    for (suffix, opclass, operator, expected) in cases {
        Spi::run(&format!(
            "CREATE TABLE hnsw_metric_{suffix}_items (
                 id integer PRIMARY KEY,
                 embedding vector NOT NULL
             );
             INSERT INTO hnsw_metric_{suffix}_items VALUES
                 (1, '[1,0]'::vector),
                 (2, '[0,1]'::vector),
                 (3, '[2,2]'::vector),
                 (4, '[3,0]'::vector);
             CREATE INDEX hnsw_metric_{suffix}_idx
                 ON hnsw_metric_{suffix}_items
                 USING pgcontext_hnsw (embedding pgcontext.{opclass});"
        ))
        .expect("dense metric HNSW fixture and index should build");

        Spi::run("SET LOCAL enable_seqscan = off; SET LOCAL enable_bitmapscan = off")
            .expect("dense metric HNSW fixture should prefer ordered index scan");
        let ids = Spi::get_one::<Vec<i32>>(&format!(
            "SELECT array_agg(id)
               FROM (
                    SELECT id
                      FROM hnsw_metric_{suffix}_items
                     ORDER BY embedding OPERATOR(pgcontext.{operator}) '[1,1]'::vector, id
                     LIMIT 4
               ) ordered"
        ))
        .expect("dense metric HNSW ordered scan should succeed")
        .unwrap_or_default();

        assert_eq!(ids, expected, "unexpected persisted {suffix} HNSW order");
        let page_visits = Spi::get_one::<i64>(
            "SELECT page_visits FROM pgcontext.hnsw_last_scan_work()",
        )
        .expect("dense metric HNSW work evidence should be readable")
        .unwrap_or_default();
        assert!(page_visits > 0, "{suffix} scan did not read persisted pages");
    }

    Spi::run(
        "SET LOCAL pgcontext.hnsw_m = 2;
         SET LOCAL pgcontext.hnsw_ef_construction = 4;
         INSERT INTO hnsw_metric_ip_items VALUES (5, '[4,4]'::vector)",
    )
    .expect("metric insert should reuse persisted build configuration");
    let ids = Spi::get_one::<Vec<i32>>(
        "SELECT array_agg(id)
           FROM (
                SELECT id
                  FROM hnsw_metric_ip_items
                 ORDER BY embedding OPERATOR(pgcontext.<#>) '[1,1]'::vector, id
                 LIMIT 5
           ) ordered",
    )
    .expect("post-build metric insert should remain searchable")
    .unwrap_or_default();
    assert_eq!(ids, vec![5, 3, 4, 1, 2]);
}

#[pg_test]
fn hnsw_scan_page_reads_are_bounded_by_directory_load_and_visited_nodes() {
    Spi::run(
        "SET LOCAL pgcontext.hnsw_m = 8;
         SET LOCAL pgcontext.hnsw_ef_construction = 32;
         SET LOCAL pgcontext.hnsw_ef_search = 32;
         CREATE TABLE hnsw_direct_read_items (
             id integer PRIMARY KEY,
             embedding vector NOT NULL
         );
         INSERT INTO hnsw_direct_read_items
         SELECT id,
                format(
                    '[%s,%s,%s,%s,%s,%s,%s,%s]',
                    id % 31, id % 29, id % 23, id % 19,
                    id % 17, id % 13, id % 11, id % 7
                )::vector
           FROM generate_series(1, 600) AS id;
         CREATE INDEX hnsw_direct_read_idx
             ON hnsw_direct_read_items
             USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ops);
         SET LOCAL enable_seqscan = off;
         SET LOCAL enable_bitmapscan = off",
    )
    .expect("direct-read HNSW fixture and index should build");

    let ids = Spi::get_one::<Vec<i32>>(
        "SELECT array_agg(id)
           FROM (
                SELECT id
                  FROM hnsw_direct_read_items
                 ORDER BY embedding OPERATOR(pgcontext.<->)
                          '[1,2,3,4,5,6,7,8]'::vector
                 LIMIT 10
           ) nearest",
    )
    .expect("direct-read HNSW query should succeed")
    .unwrap_or_default();
    assert_eq!(ids.len(), 10);

    let work = Spi::get_one::<String>(
        "SELECT format(
                    '%s,%s,%s',
                    page_visits,
                    node_reads,
                    pg_relation_size('hnsw_direct_read_idx')
                        / current_setting('block_size')::bigint
                )
           FROM pgcontext.hnsw_last_scan_work()",
    )
    .expect("direct-read HNSW work evidence should be readable")
    .expect("direct-read HNSW work evidence should be present");
    let counters = work
        .split(',')
        .map(|value| value.parse::<i64>())
        .collect::<Result<Vec<_>, _>>()
        .expect("direct-read HNSW work counters should be integers");
    let [page_visits, node_reads, relation_blocks] = counters.as_slice() else {
        panic!("unexpected direct-read HNSW work evidence: {work}");
    };
    assert!(*node_reads > 0);
    assert!(
        *page_visits <= *relation_blocks + *node_reads,
        "direct-addressed reads exceeded one directory pass plus one page per node: {work}"
    );

    let repeated_ids = Spi::get_one::<Vec<i32>>(
        "SELECT array_agg(id)
           FROM (
                SELECT id
                  FROM hnsw_direct_read_items
                 ORDER BY embedding OPERATOR(pgcontext.<->)
                          '[1,2,3,4,5,6,7,8]'::vector
                 LIMIT 10
           ) nearest",
    )
    .expect("warm direct-read HNSW query should succeed")
    .unwrap_or_default();
    assert_eq!(repeated_ids, ids);
    let warm_work = Spi::get_one::<String>(
        "SELECT format('%s,%s', page_visits, node_reads)
           FROM pgcontext.hnsw_last_scan_work()",
    )
    .expect("warm direct-read work evidence should be readable")
    .expect("warm direct-read work evidence should be present");
    let warm_counters = warm_work
        .split(',')
        .map(|value| value.parse::<i64>())
        .collect::<Result<Vec<_>, _>>()
        .expect("warm direct-read counters should be integers");
    // Warm scans serve from the backend's packed generation, so they touch
    // no relation pages at all. The original assertion here — one page visit
    // per node read — pinned the pre-packed-serving directory walk and was
    // never re-run after packed serving landed (the gate's old filter list
    // skipped this test).
    assert_eq!(
        warm_counters[0], 0,
        "a warm scan must serve from the packed generation without page reads: {warm_work}"
    );
    assert!(
        warm_counters[1] > 0,
        "a warm scan still evaluates nodes in the packed graph: {warm_work}"
    );
}

#[pg_test]
fn pgvector_hnsw_lifecycle_matches_exact_oracle_for_every_dense_metric() {
    let cases = [
        ("l2", "vector_hnsw_ops", "<->"),
        ("ip", "vector_hnsw_ip_ops", "<#>"),
        ("cosine", "vector_hnsw_cosine_ops", "<=>"),
        ("l1", "vector_hnsw_l1_ops", "<+>"),
    ];

    for (suffix, opclass, operator) in cases {
        Spi::run(&format!(
            "CREATE TABLE pgvector_hnsw_lifecycle_{suffix} (
                 id integer PRIMARY KEY,
                 embedding vector NOT NULL,
                 payload text NOT NULL
             );
             INSERT INTO pgvector_hnsw_lifecycle_{suffix} VALUES
                 (1, '[1,0]'::vector, 'one'),
                 (2, '[0,1]'::vector, 'two'),
                 (3, '[3,3]'::vector, 'tie-a'),
                 (4, '[3,3]'::vector, 'tie-b');
             CREATE INDEX pgvector_hnsw_lifecycle_{suffix}_idx
                 ON pgvector_hnsw_lifecycle_{suffix}
                 USING pgcontext_hnsw (embedding pgcontext.{opclass})"
        ))
        .expect("metric lifecycle fixture and index should build");
        assert_pgvector_hnsw_matches_exact(suffix, operator, "create");

        Spi::run(&format!(
            "INSERT INTO pgvector_hnsw_lifecycle_{suffix}
                 VALUES (5, '[4,4]'::vector, 'inserted');
             UPDATE pgvector_hnsw_lifecycle_{suffix}
                SET embedding = '[5,5]'::vector, payload = 'updated'
              WHERE id = 2;
             DELETE FROM pgvector_hnsw_lifecycle_{suffix} WHERE id = 1"
        ))
        .expect("metric lifecycle mutations should succeed");
        assert_pgvector_hnsw_matches_exact(suffix, operator, "mutations");

        Spi::run(&format!(
            "REINDEX INDEX pgvector_hnsw_lifecycle_{suffix}_idx"
        ))
        .expect("metric lifecycle index should reindex");
        assert_pgvector_hnsw_matches_exact(suffix, operator, "reindex");
    }
}

#[pg_test]
fn pgvector_hnsw_error_contract_pins_dense_metric_failures() {
    Spi::run(
        "CREATE TABLE pgvector_hnsw_error_items (
             id integer PRIMARY KEY,
             embedding vector
         );
         INSERT INTO pgvector_hnsw_error_items VALUES
             (1, '[1,0]'::vector),
             (2, NULL);
         CREATE INDEX pgvector_hnsw_error_l2_idx
             ON pgvector_hnsw_error_items
             USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ops)",
    )
    .expect("HNSW error fixture should build while ignoring NULL input");
    assert_eq!(
        Spi::get_one::<i64>(
            "SELECT count(*) FROM pgvector_hnsw_error_items WHERE embedding IS NULL"
        )
        .expect("NULL HNSW source row count should succeed"),
        Some(1)
    );

    assert_vector_compat_ddl_failure(
        "INSERT INTO pgvector_hnsw_error_items VALUES (3, '[1,0,0]'::vector)",
        "22023",
        "failed to insert HNSW graph node: dimension mismatch: left has 2 dimensions, right has 3",
        "dense HNSW dimension mismatch",
    );
    shared_assert_sql_failure(
        "SELECT '[NaN]'::vector",
        "22P02",
        "invalid vector: value at dimension 0 is not finite: NaN",
        "dense HNSW non-finite input",
    );
    shared_assert_sql_failure(
        "SELECT '[1,0]'::vector::bitvec",
        "42846",
        "cannot cast type vector to bitvec",
        "dense HNSW forbidden vector cast",
    );

    Spi::run(
        "CREATE TABLE pgvector_hnsw_error_zero (
             embedding vector NOT NULL
         );
         INSERT INTO pgvector_hnsw_error_zero VALUES ('[0,0]'::vector)",
    )
    .expect("zero-cosine fixture should be created");
    // docs/user_guide/errors.md pins `InvalidVector` to 22P02; the 22023 +
    // "failed to build HNSW graph" wrapper this test once expected predates
    // that contract and never ran under the old gate filter.
    assert_vector_compat_ddl_failure(
        "CREATE INDEX pgvector_hnsw_error_zero_idx
             ON pgvector_hnsw_error_zero
          USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops)",
        "22P02",
        "invalid vector: cosine HNSW vectors must have a finite nonzero norm",
        "dense cosine HNSW zero vector",
    );

    Spi::run(
        "CREATE OPERATOR CLASS pgvector_hnsw_error_wrong_metric_ops
             FOR TYPE pgcontext.vector USING pgcontext_hnsw AS
             OPERATOR 1 pgcontext.<#> (pgcontext.vector, pgcontext.vector) FOR ORDER BY pg_catalog.float_ops,
             FUNCTION 1 pgcontext.inner_product(pgcontext.vector, pgcontext.vector)",
    )
    .expect("wrong-metric opclass fixture should be created");
    assert_vector_compat_ddl_failure(
        "CREATE INDEX pgvector_hnsw_error_wrong_metric_idx
             ON pgvector_hnsw_error_items
          USING pgcontext_hnsw (embedding pgvector_hnsw_error_wrong_metric_ops)",
        "42P17",
        "HNSW vector opclass must use a supported pgcontext metric function",
        "dense HNSW stored metric mismatch",
    );

    Spi::run(
        "CREATE TABLE pgvector_hnsw_error_stale (
             embedding vector NOT NULL
         );
         INSERT INTO pgvector_hnsw_error_stale VALUES ('[1,0]'::vector);
         SET LOCAL pgcontext.hnsw_m = 16;
         SET LOCAL pgcontext.hnsw_ef_construction = 4",
    )
    .expect("stale configuration fixture should be created");
    assert_vector_compat_ddl_failure(
        "CREATE INDEX pgvector_hnsw_error_stale_idx
             ON pgvector_hnsw_error_stale
          USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ops)",
        "22023",
        "invalid HNSW configuration from pgContext settings: invalid HNSW parameter ef_construction: 4",
        "dense HNSW stale construction configuration",
    );
}

fn assert_pgvector_hnsw_matches_exact(suffix: &str, operator: &str, phase: &str) {
    Spi::run("SET LOCAL enable_indexscan = on; SET LOCAL enable_seqscan = off; SET LOCAL enable_bitmapscan = off")
        .expect("metric lifecycle should force HNSW index scan");
    let index_order = Spi::get_one::<Vec<i32>>(&format!(
        "SELECT array_agg(id)
           FROM (
                SELECT id
                  FROM pgvector_hnsw_lifecycle_{suffix}
                 ORDER BY embedding OPERATOR(pgcontext.{operator}) '[1,1]'::vector, id
                 LIMIT 10
           ) ordered"
    ))
    .expect("metric lifecycle HNSW query should succeed")
    .unwrap_or_default();

    Spi::run("SET LOCAL enable_indexscan = off; SET LOCAL enable_seqscan = on; SET LOCAL enable_bitmapscan = off")
        .expect("metric lifecycle should force exact sequential oracle");
    let exact_order = Spi::get_one::<Vec<i32>>(&format!(
        "SELECT array_agg(id)
           FROM (
                SELECT id
                  FROM pgvector_hnsw_lifecycle_{suffix}
                 ORDER BY embedding OPERATOR(pgcontext.{operator}) '[1,1]'::vector, id
                 LIMIT 10
           ) ordered"
    ))
    .expect("metric lifecycle exact query should succeed")
    .unwrap_or_default();

    assert_eq!(
        index_order, exact_order,
        "{suffix} HNSW differs from exact oracle after {phase}"
    );
}

#[pg_test]
fn hnsw_access_method_builds_empty_index() {
    Spi::run(
        "CREATE TABLE hnsw_empty_items (
            id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
            embedding vector NOT NULL
         )",
    )
    .expect("empty HNSW fixture table should be created");

    Spi::run(
        "CREATE INDEX hnsw_empty_items_embedding_idx
            ON hnsw_empty_items USING pgcontext_hnsw (embedding)",
    )
    .expect("empty HNSW index should build");

    let has_index = Spi::get_one::<bool>(
        "SELECT EXISTS (
            SELECT 1
              FROM pg_catalog.pg_indexes
             WHERE schemaname = 'public'
               AND tablename = 'hnsw_empty_items'
               AND indexname = 'hnsw_empty_items_embedding_idx'
        )",
    )
    .expect("empty HNSW index lookup should succeed")
    .unwrap_or_default();

    assert!(has_index);
}

#[pg_test]
fn hnsw_access_method_builds_static_table_index() {
    Spi::run(
        "CREATE TABLE hnsw_static_items (
            id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
            embedding vector NOT NULL
         )",
    )
    .expect("static HNSW fixture table should be created");

    Spi::run(
        "INSERT INTO hnsw_static_items (embedding)
         VALUES ('[1,2,3]'::vector), ('[2,3,4]'::vector), ('[3,4,5]'::vector)",
    )
    .expect("static HNSW fixture rows should be inserted");

    Spi::run(
        "CREATE INDEX hnsw_static_items_embedding_idx
            ON hnsw_static_items USING pgcontext_hnsw (embedding)",
    )
    .expect("static HNSW index should build");

    let reltuples = Spi::get_one::<f32>(
        "SELECT reltuples
           FROM pg_catalog.pg_class
          WHERE relname = 'hnsw_static_items_embedding_idx'",
    )
    .expect("static HNSW index statistics lookup should succeed")
    .unwrap_or_default();

    assert_eq!(reltuples, 3.0);
}

#[pg_test]
fn hnsw_access_method_persists_metapage_build_metadata() {
    Spi::run("CREATE EXTENSION IF NOT EXISTS pageinspect")
        .expect("pageinspect should be available for metapage inspection");
    Spi::run(
        "CREATE TABLE hnsw_meta_items (
            id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
            embedding vector NOT NULL
         )",
    )
    .expect("metapage fixture table should be created");
    Spi::run(
        "INSERT INTO hnsw_meta_items (embedding)
         VALUES ('[1,2,3]'::vector), ('[2,3,4]'::vector), ('[3,4,5]'::vector)",
    )
    .expect("metapage fixture rows should be inserted");

    Spi::run(
        "CREATE INDEX hnsw_meta_items_embedding_idx
            ON hnsw_meta_items USING pgcontext_hnsw (embedding)",
    )
    .expect("metapage HNSW index should build");

    let metadata = Spi::get_one::<String>(
        "WITH raw AS (
             SELECT get_raw_page('hnsw_meta_items_embedding_idx', 0) AS page
         ),
         item_pointer AS (
             SELECT page,
                    (
                        get_byte(page, 24) +
                        get_byte(page, 25) * 256 +
                        get_byte(page, 26) * 65536 +
                        get_byte(page, 27) * 16777216
                    ) & 32767 AS item_offset
               FROM raw
         )
         SELECT concat_ws(
                    ',',
                    get_byte(page, item_offset) +
                        get_byte(page, item_offset + 1) * 256 +
                        get_byte(page, item_offset + 2) * 65536 +
                        get_byte(page, item_offset + 3) * 16777216,
                    get_byte(page, item_offset + 8) +
                        get_byte(page, item_offset + 9) * 256 +
                        get_byte(page, item_offset + 10) * 65536 +
                        get_byte(page, item_offset + 11) * 16777216,
                    get_byte(page, item_offset + 16) +
                        get_byte(page, item_offset + 17) * 256 +
                        get_byte(page, item_offset + 18) * 65536 +
                        get_byte(page, item_offset + 19) * 16777216
                )
           FROM item_pointer",
    )
    .expect("metapage metadata query should succeed")
    .expect("metapage metadata should not be null");

    assert_eq!(metadata, "1213419095,3,3");
}

#[pg_test]
fn hnsw_settings_are_registered_and_used_for_builds() {
    Spi::run("SET LOCAL pgcontext.hnsw_m = 4").expect("hnsw_m setting should be accepted");
    Spi::run("SET LOCAL pgcontext.hnsw_ef_construction = 8")
        .expect("hnsw_ef_construction setting should be accepted");
    Spi::run("SET LOCAL pgcontext.hnsw_ef_search = 6")
        .expect("hnsw_ef_search setting should be accepted");
    Spi::run("SET LOCAL pgcontext.hnsw_candidate_budget = 64")
        .expect("hnsw_candidate_budget setting should be accepted");
    Spi::run("SET LOCAL pgcontext.hnsw_iterative_expansion_limit = 128")
        .expect("hnsw_iterative_expansion_limit setting should be accepted");
    Spi::run("SET LOCAL pgcontext.hnsw_recall_threshold = 0.9")
        .expect("hnsw_recall_threshold setting should be accepted");

    let settings = Spi::get_one::<String>(
        "SELECT concat_ws(
                    ',',
                    current_setting('pgcontext.hnsw_m'),
                    current_setting('pgcontext.hnsw_ef_construction'),
                    current_setting('pgcontext.hnsw_ef_search'),
                    current_setting('pgcontext.hnsw_candidate_budget'),
                    current_setting('pgcontext.hnsw_iterative_expansion_limit'),
                    current_setting('pgcontext.hnsw_recall_threshold')
                )",
    )
    .expect("HNSW settings query should succeed")
    .expect("HNSW settings should be present");

    assert_eq!(settings, "4,8,6,64,128,0.9");

    Spi::run(
        "CREATE TABLE hnsw_settings_items (
            id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
            embedding vector NOT NULL
         )",
    )
    .expect("settings HNSW fixture table should be created");
    Spi::run(
        "INSERT INTO hnsw_settings_items (embedding)
         VALUES ('[1,0]'::vector), ('[2,0]'::vector), ('[3,0]'::vector)",
    )
    .expect("settings HNSW fixture rows should be inserted");
    Spi::run(
        "CREATE INDEX hnsw_settings_items_embedding_idx
            ON hnsw_settings_items USING pgcontext_hnsw (embedding)",
    )
    .expect("HNSW index should build with non-default settings");
}

#[pg_test]
#[should_panic(expected = "invalid HNSW configuration from pgContext settings")]
fn hnsw_settings_reject_build_when_construction_budget_is_below_m() {
    Spi::run("SET LOCAL pgcontext.hnsw_m = 16").expect("hnsw_m setting should be accepted");
    Spi::run("SET LOCAL pgcontext.hnsw_ef_construction = 4")
        .expect("hnsw_ef_construction setting should be accepted");

    Spi::run(
        "CREATE TABLE hnsw_bad_settings_items (
            embedding vector NOT NULL
         )",
    )
    .expect("bad settings HNSW fixture table should be created");
    Spi::run(
        "INSERT INTO hnsw_bad_settings_items (embedding)
         VALUES ('[1,0]'::vector)",
    )
    .expect("bad settings HNSW fixture row should be inserted");
    Spi::run(
        "CREATE INDEX hnsw_bad_settings_items_embedding_idx
            ON hnsw_bad_settings_items USING pgcontext_hnsw (embedding)",
    )
    .expect("invalid HNSW settings should fail index build");
}

#[pg_test]
fn hnsw_quantized_scalar_index_options_are_persisted() {
    Spi::run(
        "CREATE TABLE hnsw_scalar_options_items (
            embedding vector NOT NULL
         )",
    )
    .expect("scalar options fixture table should be created");
    Spi::run(
        "INSERT INTO hnsw_scalar_options_items (embedding)
         VALUES ('[-1,0]'::vector), ('[0,1]'::vector), ('[1,0]'::vector)",
    )
    .expect("scalar options fixture rows should be inserted");

    Spi::run(
        "CREATE INDEX hnsw_scalar_options_items_embedding_idx
            ON hnsw_scalar_options_items USING pgcontext_hnsw (embedding)
          WITH (
              quantization = 'scalar',
              scalar_min = -1.0,
              scalar_max = 1.0,
              scalar_levels = 256
          )",
    )
    .expect("scalar quantized HNSW options should build");

    let options = Spi::get_one::<String>(
        "SELECT array_to_string(reloptions, ',')
           FROM pg_catalog.pg_class
          WHERE relname = 'hnsw_scalar_options_items_embedding_idx'",
    )
    .expect("scalar reloptions lookup should succeed")
    .expect("scalar reloptions should be present");

    assert!(options.contains("quantization=scalar"));
    assert!(options.contains("scalar_min=-1"));
    assert!(options.contains("scalar_max=1"));
    assert!(options.contains("scalar_levels=256"));

    Spi::run(
        "CREATE INDEX hnsw_sq8_options_items_embedding_idx
            ON hnsw_scalar_options_items USING pgcontext_hnsw (embedding)
          WITH (
              quantization = 'sq8',
              scalar_min = -2.0,
              scalar_max = 2.0,
              scalar_levels = 256
          )",
    )
    .expect("SQ8 quantized HNSW options should build");

    let sq8_options = Spi::get_one::<String>(
        "SELECT array_to_string(reloptions, ',')
           FROM pg_catalog.pg_class
          WHERE relname = 'hnsw_sq8_options_items_embedding_idx'",
    )
    .expect("SQ8 reloptions lookup should succeed")
    .expect("SQ8 reloptions should be present");

    assert!(sq8_options.contains("quantization=sq8"));
    assert!(sq8_options.contains("scalar_min=-2"));
    assert!(sq8_options.contains("scalar_max=2"));
    assert!(sq8_options.contains("scalar_levels=256"));
}

#[pg_test]
fn hnsw_quantized_pq_index_options_are_persisted() {
    Spi::run(
        "CREATE TABLE hnsw_pq_options_items (
            embedding vector NOT NULL
         )",
    )
    .expect("PQ options fixture table should be created");
    Spi::run(
        "INSERT INTO hnsw_pq_options_items (embedding)
         VALUES ('[0,0,1,1]'::vector), ('[1,1,0,0]'::vector)",
    )
    .expect("PQ options fixture rows should be inserted");

    Spi::run(
        "CREATE INDEX hnsw_pq_options_items_embedding_idx
            ON hnsw_pq_options_items USING pgcontext_hnsw (embedding)
          WITH (
              quantization = 'pq',
              pq_subvector_dimensions = 2,
              pq_codebooks = '[[[0,0],[1,1]],[[1,0],[0,1]]]'
          )",
    )
    .expect("PQ quantized HNSW options should build");

    let options = Spi::get_one::<String>(
        "SELECT array_to_string(reloptions, ',')
           FROM pg_catalog.pg_class
          WHERE relname = 'hnsw_pq_options_items_embedding_idx'",
    )
    .expect("PQ reloptions lookup should succeed")
    .expect("PQ reloptions should be present");

    assert!(options.contains("quantization=pq"));
    assert!(options.contains("pq_subvector_dimensions=2"));
    assert!(options.contains("pq_codebooks=[[[0,0],[1,1]],[[1,0],[0,1]]]"));
}

#[pg_test]
fn hnsw_quantized_pq_options_persist_metapage_metadata() {
    Spi::run("CREATE EXTENSION IF NOT EXISTS pageinspect")
        .expect("pageinspect should be available for quantized metapage inspection");
    Spi::run(
        "CREATE TABLE hnsw_pq_meta_items (
            embedding vector NOT NULL
         )",
    )
    .expect("PQ metapage fixture table should be created");
    Spi::run(
        "INSERT INTO hnsw_pq_meta_items (embedding)
         VALUES ('[0,0,1,1]'::vector), ('[1,1,0,0]'::vector)",
    )
    .expect("PQ metapage fixture rows should be inserted");

    let codebooks = "[[[0,0],[1,1]],[[1,0],[0,1]]]";
    Spi::run(&format!(
        "CREATE INDEX hnsw_pq_meta_items_embedding_idx
            ON hnsw_pq_meta_items USING pgcontext_hnsw (embedding)
          WITH (
              quantization = 'pq',
              pq_subvector_dimensions = 2,
              pq_codebooks = '{codebooks}'
          )"
    ))
    .expect("PQ quantized HNSW index should build with metapage metadata");

    let metadata = Spi::get_one::<String>(
        "WITH raw AS (
             SELECT get_raw_page('hnsw_pq_meta_items_embedding_idx', 0) AS page
         ),
         item_pointer AS (
             SELECT page,
                    (
                        get_byte(page, 24) +
                        get_byte(page, 25) * 256 +
                        get_byte(page, 26) * 65536 +
                        get_byte(page, 27) * 16777216
                    ) & 32767 AS item_offset
               FROM raw
         )
         SELECT concat_ws(
                    ',',
                    get_byte(page, item_offset + 12) +
                        get_byte(page, item_offset + 13) * 256,
                    get_byte(page, item_offset + 14) +
                        get_byte(page, item_offset + 15) * 256,
                    get_byte(page, item_offset + 52) +
                        get_byte(page, item_offset + 53) * 256 +
                        get_byte(page, item_offset + 54) * 65536 +
                        get_byte(page, item_offset + 55) * 16777216,
                    (
                        get_byte(page, item_offset + 56)::numeric +
                        get_byte(page, item_offset + 57)::numeric * 256 +
                        get_byte(page, item_offset + 58)::numeric * 65536 +
                        get_byte(page, item_offset + 59)::numeric * 16777216 +
                        get_byte(page, item_offset + 60)::numeric * 4294967296 +
                        get_byte(page, item_offset + 61)::numeric * 1099511627776 +
                        get_byte(page, item_offset + 62)::numeric * 281474976710656 +
                        get_byte(page, item_offset + 63)::numeric * 72057594037927936
                    )::text
                )
           FROM item_pointer",
    )
    .expect("PQ metapage metadata query should succeed")
    .expect("PQ metapage metadata should not be null");

    assert_eq!(
        metadata,
        format!("3,1,2,{}", fnv1a64_for_test(codebooks.as_bytes()))
    );
}

#[pg_test]
fn hnsw_quantized_index_options_reject_bad_inputs_with_sqlstate() {
    Spi::run(
        "CREATE TABLE hnsw_bad_quant_options_items (
            embedding vector NOT NULL
         )",
    )
    .expect("bad quantization options fixture table should be created");
    Spi::run(
        "INSERT INTO hnsw_bad_quant_options_items (embedding)
         VALUES ('[0,0]'::vector)",
    )
    .expect("bad quantization options fixture row should be inserted");

    for (index_name, options, expected_message) in [
        (
            "hnsw_bad_quant_mode_idx",
            "quantization = 'rotary'",
            "invalid value for enum option \"quantization\": rotary",
        ),
        (
            "hnsw_bad_scalar_range_idx",
            "quantization = 'scalar', scalar_min = 1.0, scalar_max = 1.0",
            "scalar_min must be less than scalar_max",
        ),
        (
            "hnsw_bad_scalar_levels_idx",
            "quantization = 'scalar', scalar_min = -1.0, scalar_max = 1.0, scalar_levels = 1",
            "value 1 out of bounds for option \"scalar_levels\"",
        ),
        (
            "hnsw_bad_pq_missing_codebooks_idx",
            "quantization = 'pq', pq_subvector_dimensions = 2",
            "pq_codebooks is required when quantization is pq",
        ),
        (
            "hnsw_bad_pq_codebooks_json_idx",
            "quantization = 'pq', pq_subvector_dimensions = 2, pq_codebooks = '{}'",
            "pq_codebooks must be a JSON array",
        ),
    ] {
        let sql = format!(
            "DO $$
             BEGIN
                 EXECUTE $create$
                     CREATE INDEX {index_name}
                         ON hnsw_bad_quant_options_items
                      USING pgcontext_hnsw (embedding)
                      WITH ({options})
                 $create$;
                 RAISE EXCEPTION 'expected quantized HNSW option failure';
             EXCEPTION
                 WHEN invalid_parameter_value THEN
                     IF position('{expected_message}' in SQLERRM) = 0 THEN
                         RAISE EXCEPTION 'unexpected error message: %', SQLERRM;
                     END IF;
             END
             $$"
        );

        Spi::run(&sql).expect("bad quantized HNSW option should raise 22023");
    }
}

fn fnv1a64_for_test(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[pg_test]
fn hnsw_access_method_accepts_insert_after_index_build() {
    Spi::run(
        "CREATE TABLE hnsw_insert_items (
            embedding vector NOT NULL
         )",
    )
    .expect("insert HNSW fixture table should be created");

    Spi::run(
        "CREATE INDEX hnsw_insert_items_embedding_idx
            ON hnsw_insert_items USING pgcontext_hnsw (embedding)",
    )
    .expect("insert HNSW index should build before rows exist");

    Spi::run(
        "INSERT INTO hnsw_insert_items (embedding)
         VALUES ('[1,1,1]'::vector), ('[2,2,2]'::vector)",
    )
    .expect("insert into table with HNSW index should succeed");

    let row_count = Spi::get_one::<i64>("SELECT count(*) FROM hnsw_insert_items")
        .expect("insert HNSW fixture count should succeed")
        .unwrap_or_default();

    assert_eq!(row_count, 2);
}

#[pg_test]
fn hnsw_access_method_serves_mixed_build_and_insert_records() {
    Spi::run(
        "CREATE TABLE hnsw_mixed_insert_items (
            id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
            embedding vector NOT NULL
         )",
    )
    .expect("mixed insert HNSW fixture table should be created");
    Spi::run(
        "INSERT INTO hnsw_mixed_insert_items (embedding)
         VALUES ('[100,0]'::vector), ('[90,0]'::vector)",
    )
    .expect("mixed insert HNSW fixture rows should be inserted before index build");
    Spi::run(
        "CREATE INDEX hnsw_mixed_insert_items_embedding_idx
            ON hnsw_mixed_insert_items USING pgcontext_hnsw (embedding)",
    )
    .expect("mixed insert HNSW index should build");
    Spi::run("INSERT INTO hnsw_mixed_insert_items (embedding) VALUES ('[0,0]'::vector)")
        .expect("mixed insert HNSW fixture nearest row should be inserted after index build");

    Spi::run("SET LOCAL enable_seqscan = off")
        .expect("seqscan should be disabled for mixed insert index check");
    let nearest_id = Spi::get_one::<i64>(
        "SELECT id
           FROM hnsw_mixed_insert_items
          ORDER BY embedding OPERATOR(pgcontext.<->) '[0,0]'::vector
          LIMIT 1",
    )
    .expect("mixed insert HNSW ordered query should succeed")
    .expect("mixed insert HNSW ordered query should return a row");

    assert_eq!(nearest_id, 3);
}

#[pg_test]
fn hnsw_access_method_accepts_update_after_index_build() {
    Spi::run(
        "CREATE TABLE hnsw_update_items (
            id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
            embedding vector NOT NULL
         )",
    )
    .expect("update HNSW fixture table should be created");

    Spi::run(
        "CREATE INDEX hnsw_update_items_embedding_idx
            ON hnsw_update_items USING pgcontext_hnsw (embedding)",
    )
    .expect("update HNSW index should build before rows exist");

    Spi::run("INSERT INTO hnsw_update_items (embedding) VALUES ('[1,1,1]'::vector)")
        .expect("update HNSW fixture row should be inserted");

    Spi::run("UPDATE hnsw_update_items SET embedding = '[9,9,9]'::vector WHERE id = 1")
        .expect("update through table with HNSW index should succeed");

    let distance = Spi::get_one::<f32>(
        "SELECT pgcontext.l2_distance(embedding, '[9,9,9]'::vector)
           FROM hnsw_update_items
          WHERE id = 1",
    )
    .expect("updated HNSW fixture lookup should succeed")
    .expect("updated HNSW fixture row should exist");

    assert_eq!(distance, 0.0);
}

#[pg_test]
fn hnsw_access_method_accepts_delete_after_index_build() {
    Spi::run(
        "CREATE TABLE hnsw_delete_items (
            id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
            embedding vector NOT NULL
         )",
    )
    .expect("delete HNSW fixture table should be created");

    Spi::run(
        "CREATE INDEX hnsw_delete_items_embedding_idx
            ON hnsw_delete_items USING pgcontext_hnsw (embedding)",
    )
    .expect("delete HNSW index should build before rows exist");

    Spi::run("INSERT INTO hnsw_delete_items (embedding) VALUES ('[1,1,1]'::vector)")
        .expect("delete HNSW fixture row should be inserted");

    Spi::run("DELETE FROM hnsw_delete_items WHERE id = 1")
        .expect("delete through table with HNSW index should succeed");

    let row_count = Spi::get_one::<i64>("SELECT count(*) FROM hnsw_delete_items")
        .expect("delete HNSW fixture count should succeed")
        .unwrap_or_default();

    assert_eq!(row_count, 0);
}

#[pg_test]
fn hnsw_access_method_preserves_heap_visibility_after_update_delete() {
    Spi::run(
        "CREATE TABLE hnsw_visibility_items (
            id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
            embedding vector NOT NULL
         )",
    )
    .expect("visibility HNSW fixture table should be created");

    Spi::run(
        "CREATE INDEX hnsw_visibility_items_embedding_idx
            ON hnsw_visibility_items USING pgcontext_hnsw (embedding)",
    )
    .expect("visibility HNSW index should build");

    Spi::run(
        "INSERT INTO hnsw_visibility_items (embedding)
         VALUES ('[1,1,1]'::vector), ('[2,2,2]'::vector)",
    )
    .expect("visibility HNSW fixture rows should be inserted");

    Spi::run("UPDATE hnsw_visibility_items SET embedding = '[9,9,9]'::vector WHERE id = 1")
        .expect("update through table with HNSW index should succeed");

    Spi::run("DELETE FROM hnsw_visibility_items WHERE id = 2")
        .expect("delete through table with HNSW index should succeed");

    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT count(*)::bigint,
                        COALESCE((
                            SELECT pgcontext.l2_distance(embedding, '[9,9,9]'::vector)
                              FROM hnsw_visibility_items
                             LIMIT 1
                        ), -1.0::real)",
                None,
                &[],
            )
            .expect("visibility HNSW fixture lookup should succeed");

        let row = result.first();
        Ok::<_, spi::Error>((
            row.get::<i64>(1)?.unwrap_or_default(),
            row.get::<f32>(2)?.unwrap_or_default(),
        ))
    })
    .expect("visibility HNSW fixture row should decode");

    assert_eq!(rows, (1, 0.0));

    Spi::run("SET LOCAL enable_seqscan = off")
        .expect("seqscan should be disabled for visibility index check");

    let indexed_ids = Spi::get_one::<String>(
        "SELECT string_agg(id::text, ',' ORDER BY distance_rank)
           FROM (
                SELECT id,
                       row_number() OVER (
                           ORDER BY embedding OPERATOR(pgcontext.<->) '[9,9,9]'::vector
                       ) AS distance_rank
                  FROM hnsw_visibility_items
                 ORDER BY embedding OPERATOR(pgcontext.<->) '[9,9,9]'::vector
                 LIMIT 5
           ) ordered",
    )
    .expect("visibility HNSW index query should succeed")
    .expect("visibility HNSW index query should return one visible row");

    assert_eq!(indexed_ids, "1");
}
