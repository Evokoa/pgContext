#[pg_test]
fn ivfflat_access_method_and_dense_opclasses_are_registered() {
    let access_method = Spi::get_one::<String>(
        "SELECT amtype::text FROM pg_catalog.pg_am WHERE amname = 'pgcontext_ivfflat'",
    )
    .expect("IVFFlat access-method lookup should run");
    assert_eq!(access_method.as_deref(), Some("i"));

    let opclasses = Spi::get_one::<Vec<String>>(
        "SELECT array_agg(opc.opcname::text ORDER BY opc.opcname)
           FROM pg_catalog.pg_opclass opc
           JOIN pg_catalog.pg_am am ON am.oid = opc.opcmethod
           JOIN pg_catalog.pg_namespace nsp ON nsp.oid = opc.opcnamespace
          WHERE am.amname = 'pgcontext_ivfflat'
            AND nsp.nspname = 'pgcontext'
            AND opc.opcintype = 'pgcontext.vector'::regtype",
    )
    .expect("IVFFlat opclass lookup should run")
    .unwrap_or_default();
    assert_eq!(
        opclasses,
        vec![
            "vector_ivfflat_cosine_ops".to_owned(),
            "vector_ivfflat_ip_ops".to_owned(),
            "vector_ivfflat_l1_ops".to_owned(),
            "vector_ivfflat_ops".to_owned(),
        ]
    );
}

#[pg_test]
fn ivfflat_lists_reloption_and_forced_ordered_scan_match_exact_source() {
    Spi::run(
        "CREATE TABLE ivfflat_dense_items (
             id integer PRIMARY KEY,
             embedding pgcontext.vector(2)
         );
         INSERT INTO ivfflat_dense_items VALUES
             (1, '[0,0]'),
             (2, '[0.1,0]'),
             (3, '[9.9,10]'),
             (4, '[10,10]'),
             (5, NULL);
         CREATE INDEX ivfflat_dense_idx
             ON ivfflat_dense_items
             USING pgcontext_ivfflat
                   (embedding pgcontext.vector_ivfflat_ops)
             WITH (lists = 2);
         SET LOCAL pgcontext.ivfflat_probes = 2;
         SET LOCAL enable_seqscan = off;
         SET LOCAL enable_bitmapscan = off",
    )
    .expect("IVFFlat fixture and index should build");

    let plan = Spi::connect(|client| {
        let result = client.select(
            "EXPLAIN (FORMAT TEXT)
             SELECT id
               FROM ivfflat_dense_items
              ORDER BY embedding OPERATOR(pgcontext.<->) '[0,0]'::pgcontext.vector
              LIMIT 4",
            None,
            &[],
        )?;
        let mut lines = Vec::new();
        for row in result {
            lines.push(row.get::<String>(1)?.unwrap_or_default());
        }
        Ok::<_, spi::Error>(lines.join("\n"))
    })
    .expect("IVFFlat EXPLAIN should run");
    assert!(plan.contains("ivfflat_dense_idx"), "unexpected plan: {plan}");

    let ids = Spi::get_one::<Vec<i32>>(
        "SELECT array_agg(id)
           FROM (
                SELECT id
                  FROM ivfflat_dense_items
                 WHERE embedding IS NOT NULL
                 ORDER BY embedding OPERATOR(pgcontext.<->) '[0,0]'::pgcontext.vector, id
                 LIMIT 4
           ) nearest",
    )
    .expect("IVFFlat ordered scan should run")
    .unwrap_or_default();
    assert_eq!(ids, vec![1, 2, 3, 4]);

    let visited_lists = Spi::get_one::<i64>(
        "SELECT visited_lists FROM pgcontext.ivfflat_last_scan_work()",
    )
    .expect("IVFFlat scan diagnostics should execute");
    assert_eq!(visited_lists, Some(2));

    let verified = Spi::get_one::<bool>(
        "SELECT (pgcontext.ivfflat_index_info('ivfflat_dense_idx'::regclass)->>'verified')::boolean",
    )
    .expect("IVFFlat index verifier should execute");
    assert_eq!(verified, Some(true));
}

#[pg_test]
fn ivfflat_iterative_scan_widens_only_after_post_filter_exhaustion() {
    Spi::run(
        "CREATE TABLE ivfflat_filtered_items (
             id integer PRIMARY KEY,
             tenant integer NOT NULL,
             embedding pgcontext.vector(2) NOT NULL
         );
         INSERT INTO ivfflat_filtered_items VALUES
             (1, 1, '[0,0]'),
             (2, 1, '[0.1,0]'),
             (3, 2, '[10,10]'),
             (4, 2, '[10.1,10]');
         CREATE INDEX ivfflat_filtered_idx
             ON ivfflat_filtered_items
             USING pgcontext_ivfflat
                   (embedding pgcontext.vector_ivfflat_ops)
             WITH (lists = 2);
         SET LOCAL enable_seqscan = off;
         SET LOCAL enable_bitmapscan = off;
         SET LOCAL pgcontext.ivfflat_probes = 1;
         SET LOCAL pgcontext.ivfflat_max_probes = 2;
         SET LOCAL pgcontext.ivfflat_candidate_budget = 16;
         SET LOCAL pgcontext.ivfflat_iterative_scan = relaxed_order",
    )
    .expect("filtered IVFFlat fixture should build");

    let id = Spi::get_one::<i32>(
        "SELECT id
           FROM ivfflat_filtered_items
          WHERE tenant = 2
          ORDER BY embedding OPERATOR(pgcontext.<->) '[0,0]'::pgcontext.vector
          LIMIT 1",
    )
    .expect("filtered IVFFlat query should execute");
    assert_eq!(id, Some(3));

    let diagnostics = Spi::get_one::<pgrx::JsonB>(
        "SELECT to_jsonb(work)
           FROM pgcontext.ivfflat_last_scan_work() AS work",
    )
    .expect("iterative diagnostics should execute")
    .expect("iterative diagnostics should return one row");
    assert_eq!(diagnostics.0["requested_probes"], 2);
    assert_eq!(diagnostics.0["widening_rounds"], 1);
    assert_eq!(diagnostics.0["completion_reason"], "all_lists");
    assert_eq!(diagnostics.0["visited_lists"], 2);
    assert_eq!(diagnostics.0["visited_postings"], 4);
    assert_eq!(diagnostics.0["delta_records"], 0);
    assert_eq!(diagnostics.0["exact_rerank_candidates"], 3);
}

#[pg_test]
fn ivfflat_relaxed_widening_applies_delta_and_tombstones_once() {
    Spi::run(
        "CREATE TABLE ivfflat_delta_widen_items (
             id integer PRIMARY KEY,
             tenant integer NOT NULL,
             embedding pgcontext.vector(2) NOT NULL
         );
         INSERT INTO ivfflat_delta_widen_items VALUES
             (1, 1, '[0,0]'),
             (2, 1, '[0.1,0]'),
             (3, 2, '[10,10]'),
             (4, 2, '[10.1,10]');
         CREATE INDEX ivfflat_delta_widen_idx
             ON ivfflat_delta_widen_items
             USING pgcontext_ivfflat
                   (embedding pgcontext.vector_ivfflat_ops)
             WITH (lists = 2);
         SELECT pgcontext.ivfflat_test_append_tombstone(
                    'ivfflat_delta_widen_idx'::regclass,
                    ctid
                )
           FROM ivfflat_delta_widen_items
          WHERE id = 1;
         DELETE FROM ivfflat_delta_widen_items WHERE id = 1;
         INSERT INTO ivfflat_delta_widen_items VALUES (5, 2, '[0.05,0]');
         SET LOCAL enable_seqscan = off;
         SET LOCAL enable_bitmapscan = off;
         SET LOCAL pgcontext.ivfflat_probes = 1;
         SET LOCAL pgcontext.ivfflat_max_probes = 2;
         SET LOCAL pgcontext.ivfflat_candidate_budget = 6;
         SET LOCAL pgcontext.ivfflat_iterative_scan = relaxed_order",
    )
    .expect("delta widening fixture should build");

    let ids = Spi::get_one::<Vec<i32>>(
        "SELECT array_agg(id)
           FROM (
                SELECT id
                  FROM ivfflat_delta_widen_items
                 WHERE tenant = 2
                 ORDER BY embedding OPERATOR(pgcontext.<->) '[0,0]'::pgcontext.vector
                 LIMIT 3
           ) nearest",
    )
    .expect("delta widening query should execute")
    .unwrap_or_default();
    assert_eq!(ids, vec![5, 3, 4]);
    assert_eq!(ids.iter().filter(|id| **id == 5).count(), 1);
    assert!(!ids.contains(&1), "tombstoned base tuple was returned");

    let diagnostics = Spi::get_one::<pgrx::JsonB>(
        "SELECT to_jsonb(work) FROM pgcontext.ivfflat_last_scan_work() AS work",
    )
    .expect("delta widening diagnostics should execute")
    .expect("delta widening diagnostics should return one row");
    assert_eq!(diagnostics.0["requested_probes"], 2);
    assert_eq!(diagnostics.0["widening_rounds"], 1);
    assert_eq!(diagnostics.0["visited_postings"], 4);
    assert_eq!(diagnostics.0["delta_records"], 2);
    assert_eq!(
        diagnostics.0["visited_postings"].as_u64().unwrap_or_default()
            + diagnostics.0["delta_records"].as_u64().unwrap_or_default(),
        6,
        "the scan-global budget must charge unique base postings plus persisted deltas",
    );
}

#[pg_test]
fn ivfflat_relaxed_limit_stops_after_the_initial_probe_batch() {
    Spi::run(
        "CREATE TABLE ivfflat_relaxed_limit_items (
             id integer PRIMARY KEY,
             embedding pgcontext.vector(2) NOT NULL
         );
         INSERT INTO ivfflat_relaxed_limit_items VALUES
             (1, '[0,0]'), (2, '[0.1,0]'), (3, '[10,10]'), (4, '[10.1,10]');
         CREATE INDEX ivfflat_relaxed_limit_idx
             ON ivfflat_relaxed_limit_items
             USING pgcontext_ivfflat
                   (embedding pgcontext.vector_ivfflat_ops)
             WITH (lists = 2);
         SET LOCAL enable_seqscan = off;
         SET LOCAL enable_bitmapscan = off;
         SET LOCAL pgcontext.ivfflat_probes = 1;
         SET LOCAL pgcontext.ivfflat_max_probes = 2;
         SET LOCAL pgcontext.ivfflat_candidate_budget = 16;
         SET LOCAL pgcontext.ivfflat_iterative_scan = relaxed_order",
    )
    .expect("relaxed IVFFlat fixture should build");

    let id = Spi::get_one::<i32>(
        "SELECT id
           FROM ivfflat_relaxed_limit_items
          ORDER BY embedding OPERATOR(pgcontext.<->) '[0,0]'::pgcontext.vector
          LIMIT 1",
    )
    .expect("relaxed initial batch should execute");
    assert_eq!(id, Some(1));
    let diagnostics = Spi::get_one::<pgrx::JsonB>(
        "SELECT to_jsonb(work) FROM pgcontext.ivfflat_last_scan_work() AS work",
    )
    .expect("relaxed diagnostics should execute")
    .expect("relaxed diagnostics should return one row");
    assert_eq!(diagnostics.0["requested_probes"], 1);
    assert_eq!(diagnostics.0["visited_lists"], 1);
    assert_eq!(diagnostics.0["widening_rounds"], 0);
}

#[pg_test]
#[should_panic(expected = "IVFFlat candidate budget 3 exhausted after 4 postings")]
fn ivfflat_relaxed_widening_enforces_one_scan_global_budget() {
    Spi::run(
        "CREATE TABLE ivfflat_global_budget_items (
             id integer PRIMARY KEY,
             tenant integer NOT NULL,
             embedding pgcontext.vector(2) NOT NULL
         );
         INSERT INTO ivfflat_global_budget_items VALUES
             (1, 1, '[0,0]'), (2, 1, '[0.1,0]'),
             (3, 2, '[10,10]'), (4, 2, '[10.1,10]');
         CREATE INDEX ivfflat_global_budget_idx
             ON ivfflat_global_budget_items
             USING pgcontext_ivfflat
                   (embedding pgcontext.vector_ivfflat_ops)
             WITH (lists = 2);
         SET LOCAL enable_seqscan = off;
         SET LOCAL enable_bitmapscan = off;
         SET LOCAL pgcontext.ivfflat_probes = 1;
         SET LOCAL pgcontext.ivfflat_max_probes = 2;
         SET LOCAL pgcontext.ivfflat_candidate_budget = 3;
         SET LOCAL pgcontext.ivfflat_iterative_scan = relaxed_order;
         SELECT id
           FROM ivfflat_global_budget_items
          WHERE tenant = 2
          ORDER BY embedding OPERATOR(pgcontext.<->) '[0,0]'::pgcontext.vector
          LIMIT 1",
    )
    .expect("scan-global candidate budget should fail closed");
}

#[pg_test]
fn ivfflat_quantized_spill_fan_in_is_cardinality_independent() {
    let (peak, retained) = crate::ivfflat_am::test_ivfflat_parallel_transcode_bound(16_384);
    let word_bound = usize::BITS as usize;
    assert!(retained <= word_bound, "retained {retained} spill runs");
    assert!(peak <= word_bound + 2, "peak {peak} live spill files");
}

#[pg_test]
fn ivfflat_strict_order_materializes_the_bounded_frontier() {
    Spi::run(
        "CREATE TABLE ivfflat_strict_items (
             id integer PRIMARY KEY,
             embedding pgcontext.vector(2) NOT NULL
         );
         INSERT INTO ivfflat_strict_items VALUES
             (1, '[0,0]'), (2, '[0.1,0]'), (3, '[10,10]'), (4, '[10.1,10]');
         CREATE INDEX ivfflat_strict_idx
             ON ivfflat_strict_items
             USING pgcontext_ivfflat
                   (embedding pgcontext.vector_ivfflat_ops)
             WITH (lists = 2);
         SET LOCAL enable_seqscan = off;
         SET LOCAL enable_bitmapscan = off;
         SET LOCAL pgcontext.ivfflat_probes = 1;
         SET LOCAL pgcontext.ivfflat_max_probes = 2;
         SET LOCAL pgcontext.ivfflat_candidate_budget = 16;
         SET LOCAL pgcontext.ivfflat_iterative_scan = strict_order",
    )
    .expect("strict IVFFlat fixture should build");

    let ids = Spi::get_one::<Vec<i32>>(
        "SELECT array_agg(id ORDER BY distance, id)
           FROM (
             SELECT id,
                    embedding OPERATOR(pgcontext.<->) '[0,0]'::pgcontext.vector AS distance
               FROM ivfflat_strict_items
              ORDER BY embedding OPERATOR(pgcontext.<->) '[0,0]'::pgcontext.vector
              LIMIT 4
           ) ranked",
    )
    .expect("strict IVFFlat query should execute");
    assert_eq!(ids, Some(vec![1, 2, 3, 4]));

    let diagnostics = Spi::get_one::<pgrx::JsonB>(
        "SELECT to_jsonb(work)
           FROM pgcontext.ivfflat_last_scan_work() AS work",
    )
    .expect("strict IVFFlat diagnostics should execute")
    .expect("strict IVFFlat diagnostics should return one row");
    assert_eq!(diagnostics.0["requested_probes"], 2);
    assert_eq!(diagnostics.0["widening_rounds"], 0);
}

#[pg_test]
#[should_panic(expected = "value 0 out of bounds for option \"lists\"")]
fn ivfflat_lists_reloption_rejects_zero() {
    Spi::run(
        "CREATE TABLE ivfflat_invalid_lists (embedding pgcontext.vector(2));
         CREATE INDEX ivfflat_invalid_lists_idx
             ON ivfflat_invalid_lists
             USING pgcontext_ivfflat
                   (embedding pgcontext.vector_ivfflat_ops)
             WITH (lists = 0)",
    )
    .expect("invalid IVFFlat lists should be rejected by PostgreSQL");
}

#[pg_test]
#[should_panic(expected = "IVFFlat SQ8/PQ quantization requires a continuous dense metric")]
fn ivfflat_quantization_rejects_binary_metrics() {
    Spi::run(
        "CREATE TABLE ivfflat_invalid_codec (embedding pgcontext.bitvec(8));
         INSERT INTO ivfflat_invalid_codec VALUES ('00000000'), ('11111111');
         CREATE INDEX ivfflat_invalid_codec_idx
             ON ivfflat_invalid_codec
             USING pgcontext_ivfflat
                   (embedding pgcontext.bitvec_ivfflat_hamming_ops)
             WITH (lists = 2, quantization = sq8)",
    )
    .expect("IVFFlat quantization should reject binary source metrics");
}

#[pg_test]
fn ivfflat_incremental_dml_preserves_visible_exact_order() {
    Spi::run(
        "CREATE TABLE ivfflat_dml_items (
             id integer PRIMARY KEY,
             embedding pgcontext.vector(2)
         );
         CREATE INDEX ivfflat_dml_idx
             ON ivfflat_dml_items
             USING pgcontext_ivfflat
                   (embedding pgcontext.vector_ivfflat_ops)
             WITH (lists = 2);
         INSERT INTO ivfflat_dml_items VALUES
             (1, '[0,0]'), (2, '[1,0]'), (3, '[2,0]');
         UPDATE ivfflat_dml_items SET embedding = '[0.25,0]' WHERE id = 3;
         DELETE FROM ivfflat_dml_items WHERE id = 2;
         SET LOCAL pgcontext.ivfflat_probes = 2;
         SET LOCAL enable_seqscan = off;
         SET LOCAL enable_bitmapscan = off",
    )
    .expect("IVFFlat incremental DML should succeed");

    let ids = Spi::get_one::<Vec<i32>>(
        "SELECT array_agg(id)
           FROM (
                SELECT id
                  FROM ivfflat_dml_items
                 ORDER BY embedding OPERATOR(pgcontext.<->) '[0,0]'::pgcontext.vector, id
           ) ordered",
    )
    .expect("IVFFlat post-DML scan should succeed")
    .unwrap_or_default();
    assert_eq!(ids, vec![1, 3]);
}

#[pg_test]
fn ivfflat_compaction_folds_delta_into_a_new_verified_generation() {
    Spi::run(
        "CREATE TABLE ivfflat_compact_items (
             id integer PRIMARY KEY,
             embedding pgcontext.vector(2) NOT NULL
         );
         INSERT INTO ivfflat_compact_items VALUES
             (1, '[0,0]'), (2, '[1,0]'), (3, '[2,0]');
         CREATE INDEX ivfflat_compact_idx
             ON ivfflat_compact_items
             USING pgcontext_ivfflat
                   (embedding pgcontext.vector_ivfflat_ops)
             WITH (lists = 2);
         UPDATE ivfflat_compact_items SET embedding = '[0.25,0]' WHERE id = 3;
         DELETE FROM ivfflat_compact_items WHERE id = 2",
    )
    .expect("IVFFlat compaction fixture should build");

    let before = Spi::get_one::<pgrx::JsonB>(
        "SELECT pgcontext.ivfflat_index_info('ivfflat_compact_idx'::regclass)",
    )
    .expect("pre-compaction verifier should execute")
    .expect("pre-compaction verifier should return JSON");
    assert!(before.0["delta_records"].as_u64().unwrap_or_default() >= 1);

    let compacted = Spi::get_one::<pgrx::JsonB>(
        "SELECT pgcontext.compact_ivfflat('ivfflat_compact_idx'::regclass)",
    )
    .expect("IVFFlat compaction should execute")
    .expect("IVFFlat compaction should report publication");
    assert_eq!(compacted.0["folded_delta_records"], before.0["delta_records"]);

    let after = Spi::get_one::<pgrx::JsonB>(
        "SELECT pgcontext.ivfflat_index_info('ivfflat_compact_idx'::regclass)",
    )
    .expect("post-compaction verifier should execute")
    .expect("post-compaction verifier should return JSON");
    assert_eq!(after.0["verified"], true);
    assert_eq!(after.0["delta_records"], 0);
    assert!(after.0["generation"].as_u64() > before.0["generation"].as_u64());
    assert_eq!(after.0["base_tuples"], 2);

    let first_size = Spi::get_one::<i64>(
        "SELECT pg_relation_size('ivfflat_compact_idx'::regclass)::bigint",
    )
    .expect("first compacted relation size should be readable")
    .unwrap_or_default();
    let retired = Spi::get_one::<pgrx::JsonB>(
        "SELECT pgcontext.compact_ivfflat('ivfflat_compact_idx'::regclass)",
    )
    .expect("second IVFFlat compaction should execute")
    .expect("second IVFFlat compaction should report retirement");
    let retired_size = Spi::get_one::<i64>(
        "SELECT pg_relation_size('ivfflat_compact_idx'::regclass)::bigint",
    )
    .expect("retired IVFFlat relation size should be readable")
    .unwrap_or_default();
    assert!(retired.0["reclaimed_pages"].as_u64().unwrap_or_default() > 0);
    assert!(retired_size < first_size);
}

#[pg_test]
fn ivfflat_compaction_enforces_maintenance_privilege() {
    sql_test_create_role("ivfflat_compact_owner");
    sql_test_create_role("ivfflat_compact_maintainer");
    sql_test_create_role("ivfflat_compact_denied");
    for role in [
        "ivfflat_compact_owner",
        "ivfflat_compact_maintainer",
        "ivfflat_compact_denied",
    ] {
        sql_test_grant_api_access(role);
    }
    Spi::run(
        "CREATE TABLE ivfflat_compact_acl_items (
             id integer PRIMARY KEY,
             embedding pgcontext.vector(2) NOT NULL
         );
         INSERT INTO ivfflat_compact_acl_items VALUES
             (1, '[0,0]'), (2, '[1,1]'), (3, '[2,2]'), (4, '[3,3]');
         CREATE INDEX ivfflat_compact_acl_idx
             ON ivfflat_compact_acl_items
             USING pgcontext_ivfflat
                   (embedding pgcontext.vector_ivfflat_ops)
             WITH (lists = 2);
         ALTER TABLE ivfflat_compact_acl_items OWNER TO ivfflat_compact_owner;
         GRANT MAINTAIN ON ivfflat_compact_acl_items TO ivfflat_compact_maintainer",
    )
    .expect("IVFFlat compaction ACL fixture should build");

    sql_test_set_session_user("ivfflat_compact_denied");
    shared_assert_sql_failure(
        "SELECT pgcontext.compact_ivfflat('ivfflat_compact_acl_idx'::regclass)",
        "42501",
        "permission denied for IVFFlat index maintenance",
        "unauthorized IVFFlat compaction",
    );
    sql_test_reset_session_user();

    sql_test_set_session_user("ivfflat_compact_maintainer");
    let maintained = Spi::get_one::<pgrx::JsonB>(
        "SELECT pgcontext.compact_ivfflat('ivfflat_compact_acl_idx'::regclass)",
    )
    .expect("MAINTAIN-authorized compaction should execute")
    .expect("MAINTAIN-authorized compaction should return diagnostics");
    assert_eq!(maintained.0["source_tuples"], 4);
    sql_test_reset_session_user();

    sql_test_set_session_user("ivfflat_compact_owner");
    let owned = Spi::get_one::<pgrx::JsonB>(
        "SELECT pgcontext.compact_ivfflat('ivfflat_compact_acl_idx'::regclass)",
    )
    .expect("owner compaction should execute")
    .expect("owner compaction should return diagnostics");
    assert_eq!(owned.0["source_tuples"], 4);
    sql_test_reset_session_user();
}

#[pg_test]
fn ivfflat_supervised_compaction_completes_the_durable_job_lifecycle() {
    Spi::run(
        "CREATE TABLE ivfflat_supervised_items (
             id integer PRIMARY KEY,
             embedding pgcontext.vector(2) NOT NULL
         );
         INSERT INTO ivfflat_supervised_items VALUES
             (1, '[0,0]'), (2, '[1,0]'), (3, '[2,0]');
         SELECT pgcontext.create_collection(
             'ivfflat_supervised_items', 'public.ivfflat_supervised_items'
         );
         CREATE INDEX ivfflat_supervised_idx
             ON ivfflat_supervised_items
             USING pgcontext_ivfflat
                   (embedding pgcontext.vector_ivfflat_ops)
             WITH (lists = 2);
         UPDATE ivfflat_supervised_items SET embedding = '[0.25,0]' WHERE id = 3",
    )
    .expect("supervised IVFFlat fixture should build");

    let job_id = Spi::get_one::<i64>(
        "SELECT build_job_id
           FROM pgcontext.enqueue_ivfflat_compaction(
               'ivfflat_supervised_items', 'ivfflat_supervised_idx'
           )",
    )
    .expect("IVFFlat compaction should enqueue")
    .expect("IVFFlat compaction job id should not be null");
    for _ in 0..4 {
        assert!(
            crate::build_worker::process_one_step(),
            "each IVFFlat supervised lifecycle step should progress"
        );
    }
    let status = Spi::get_one_with_args::<String>(
        "SELECT status::text
           FROM pgcontext._build_jobs
          WHERE build_job_id = $1",
        &[job_id.into()],
    )
    .expect("IVFFlat compaction job status should be readable");
    assert_eq!(status.as_deref(), Some("completed"));
    let delta_records = Spi::get_one::<i64>(
        "SELECT (pgcontext.ivfflat_index_info(
                    'ivfflat_supervised_idx'::regclass
                )->>'delta_records')::bigint",
    )
    .expect("supervised IVFFlat publication should verify");
    assert_eq!(delta_records, Some(0));
}

#[pg_test]
fn ivfflat_incremental_dml_spans_multiple_wal_pages() {
    Spi::run(
        "CREATE TABLE ivfflat_wide_items (
             id integer PRIMARY KEY,
             embedding pgcontext.vector(3000) NOT NULL
         );
         CREATE INDEX ivfflat_wide_idx
             ON ivfflat_wide_items
             USING pgcontext_ivfflat
                   (embedding pgcontext.vector_ivfflat_ops)
             WITH (lists = 2);
         INSERT INTO ivfflat_wide_items
         SELECT 1, ('[' || string_agg('0', ',') || ']')::pgcontext.vector
           FROM generate_series(1, 3000);
         SET LOCAL enable_seqscan = off;
         SET LOCAL enable_bitmapscan = off",
    )
    .expect("wide incremental IVFFlat vector should span WAL pages");

    let id = Spi::get_one::<i32>(
        "SELECT id
           FROM ivfflat_wide_items
          ORDER BY embedding OPERATOR(pgcontext.<->)
                   (SELECT ('[' || string_agg('0', ',') || ']')::pgcontext.vector
                      FROM generate_series(1, 3000))
          LIMIT 1",
    )
    .expect("wide IVFFlat index scan should execute");
    assert_eq!(id, Some(1));
}

#[pg_test]
fn ivfflat_external_parallel_build_runs_under_low_memory() {
    Spi::run(
        "SET LOCAL maintenance_work_mem = '1MB';
         SET LOCAL max_parallel_workers = 4;
         SET LOCAL max_parallel_maintenance_workers = 4;
         SET LOCAL min_parallel_table_scan_size = 0;
         SET LOCAL pgcontext.ivfflat_build_parallel_workers = 4;
         CREATE TABLE ivfflat_external_items AS
         SELECT id,
                format('[%s,%s,%s,%s]', id%31, id%29, id%23, id%19)::pgcontext.vector(4)
                    AS embedding
           FROM generate_series(1, 10000) id;
         ALTER TABLE ivfflat_external_items SET (parallel_workers = 4);
         CREATE INDEX ivfflat_external_idx
             ON ivfflat_external_items
             USING pgcontext_ivfflat
                   (embedding pgcontext.vector_ivfflat_ops)
             WITH (lists = 32)",
    )
    .expect("low-memory external parallel IVFFlat build should succeed");

    let verified = Spi::get_one::<bool>(
        "SELECT (pgcontext.ivfflat_index_info('ivfflat_external_idx'::regclass)->>'verified')::boolean",
    )
    .expect("external IVFFlat index verifier should execute");
    assert_eq!(verified, Some(true));
    let workers = Spi::get_one::<i32>(
        "SELECT (pgcontext.ivfflat_index_info('ivfflat_external_idx'::regclass)->>'build_workers')::integer",
    )
    .expect("external IVFFlat worker count should be readable");
    assert!(workers.is_some_and(|workers| workers >= 1));
}

#[pg_test]
fn ivfflat_sq8_and_pq_use_compact_codes_with_exact_source_order() {
    Spi::run(
        "CREATE TABLE ivfflat_quantized_items AS
         SELECT id,
                format('[%s,%s,%s,%s,%s,%s,%s,%s]',
                       sin(id::double precision), cos(id::double precision),
                       id%17, id%13, id%11, id%7, id%5, id%3)::pgcontext.vector(8)
                    AS embedding
           FROM generate_series(1, 512) id;
         SET LOCAL pgcontext.ivfflat_probes = 8;
         SET LOCAL enable_bitmapscan = off",
    )
    .expect("quantized IVFFlat fixture should build");

    let ranked = "SELECT array_agg(id) FROM (
         SELECT id FROM ivfflat_quantized_items
          ORDER BY embedding OPERATOR(pgcontext.<->)
                   '[0,1,0,0,0,0,0,0]'::pgcontext.vector, id
          LIMIT 25
     ) ranked";
    Spi::run("SET LOCAL enable_indexscan = off; SET LOCAL enable_seqscan = on")
        .expect("quantized exact oracle should force a sequential scan");
    let exact = Spi::get_one::<Vec<i32>>(ranked)
        .expect("quantized exact oracle should execute")
        .unwrap_or_default();

    for (suffix, options, expected_codec) in [
        ("sq8", "lists = 8, quantization = sq8", "sq8"),
        (
            "pq",
            "lists = 8, quantization = pq, pq_subvector_dimensions = 2",
            "pq",
        ),
    ] {
        let index = format!("ivfflat_quantized_{suffix}_idx");
        Spi::run(&format!(
            "CREATE INDEX {index} ON ivfflat_quantized_items
                 USING pgcontext_ivfflat (embedding pgcontext.vector_ivfflat_ops)
                 WITH ({options});
             SET LOCAL enable_indexscan = on;
             SET LOCAL enable_seqscan = off"
        ))
        .expect("quantized IVFFlat index should build");
        let actual = Spi::get_one::<Vec<i32>>(ranked)
            .expect("quantized IVFFlat scan should execute")
            .unwrap_or_default();
        assert_eq!(actual, exact, "{suffix} exact source order drifted");

        let codec = Spi::get_one::<String>(&format!(
            "SELECT pgcontext.ivfflat_index_info('{index}'::regclass)->>'codec'"
        ))
        .expect("quantized IVFFlat verifier should execute");
        assert_eq!(codec.as_deref(), Some(expected_codec));
        let code_width = Spi::get_one::<i32>(&format!(
            "SELECT (pgcontext.ivfflat_index_info('{index}'::regclass)->>'codec_code_width')::integer"
        ))
        .expect("quantized IVFFlat code width should be reported");
        assert!(code_width.unwrap_or(0) > 0);
        Spi::run(&format!("DROP INDEX {index}"))
            .expect("quantized IVFFlat index should drop");
    }
}

#[pg_test]
fn ivfflat_native_source_representations_match_exact_oracles() {
    Spi::run(
        "CREATE TABLE ivfflat_native_items (
             id integer PRIMARY KEY,
             half_value pgcontext.halfvec(4) NOT NULL,
             int_value pgcontext.int8vec(4) NOT NULL,
             uint_value pgcontext.uint8vec(4) NOT NULL,
             bit_value pgcontext.bitvec(4) NOT NULL
         );
         INSERT INTO ivfflat_native_items VALUES
             (1, '[0,0,0,0]', '[0,0,0,0]', '[0,0,0,0]', '0000'),
             (2, '[1,1,1,1]', '[1,1,1,1]', '[1,1,1,1]', '0001'),
             (3, '[4,4,4,4]', '[4,4,4,4]', '[4,4,4,4]', '1111');
         SET LOCAL pgcontext.ivfflat_probes = 2",
    )
    .expect("native IVFFlat fixture should build");

    let cases = [
        (
            "half",
            "half_value",
            "halfvec_ivfflat_ops",
            "OPERATOR(pgcontext.<->)",
            "pgcontext.halfvec('[0,0,0,0]')",
        ),
        (
            "int8",
            "int_value",
            "int8vec_ivfflat_ops",
            "OPERATOR(pgcontext.<->)",
            "pgcontext.int8vec('[0,0,0,0]')",
        ),
        (
            "uint8",
            "uint_value",
            "uint8vec_ivfflat_ops",
            "OPERATOR(pgcontext.<->)",
            "pgcontext.uint8vec('[0,0,0,0]')",
        ),
        (
            "bit",
            "bit_value",
            "bitvec_ivfflat_hamming_ops",
            "OPERATOR(pgcontext.<~>)",
            "pgcontext.bitvec('0000')",
        ),
    ];
    for (suffix, column, opclass, operator, query) in cases {
        let ranked = format!(
            "SELECT array_agg(id) FROM (
                 SELECT id FROM ivfflat_native_items
                  ORDER BY {column} {operator} {query}, id
             ) ranked"
        );
        Spi::run(
            "SET LOCAL enable_indexscan = off;
             SET LOCAL enable_seqscan = on",
        )
        .expect("native exact oracle should force a sequential scan");
        let exact = Spi::get_one::<Vec<i32>>(&ranked)
            .expect("native exact oracle should execute")
            .unwrap_or_default();
        let index = format!("ivfflat_native_{suffix}_idx");
        Spi::run(&format!(
            "CREATE INDEX {index} ON ivfflat_native_items
                 USING pgcontext_ivfflat ({column} pgcontext.{opclass})
                 WITH (lists = 2);
             SET LOCAL enable_indexscan = on;
             SET LOCAL enable_seqscan = off;
             SET LOCAL enable_bitmapscan = off"
        ))
        .expect("native IVFFlat index should build");
        let actual = Spi::get_one::<Vec<i32>>(&ranked)
            .expect("native IVFFlat scan should execute")
            .unwrap_or_default();
        assert_eq!(actual, exact, "native {suffix} IVFFlat order drifted");
        Spi::run(&format!("DROP INDEX {index}"))
            .expect("native IVFFlat index should drop");
    }
}
