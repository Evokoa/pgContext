// Security boundaries of the membership-filtered PUBLIC visibility views:
// leaky-qual resistance (security_barrier), owner-role visibility of automatic
// query stats, and privilege gating of the telemetry queue-health surface.
// Shares the `acl_*` / `shared_assert_sql_failure` helpers defined alongside the
// other pg_tests modules (all are `include!`d into one test module).

#[pg_test]
fn hostile_visibility_predicate_cannot_observe_another_tenants_source_keys() {
    acl_create_role("m1_visibility_owner");
    acl_create_role("m1_visibility_outsider");
    acl_grant_api_access("m1_visibility_owner");
    acl_grant_api_access("m1_visibility_outsider");

    acl_set_session_user("m1_visibility_owner");
    Spi::run(
        "CREATE TABLE public.m1_visibility_docs (
             id text PRIMARY KEY,
             embedding vector
         );
         INSERT INTO public.m1_visibility_docs (id, embedding)
         VALUES ('tenant-secret-source-key', '[1,0]'::vector);
         SELECT pgcontext.create_collection(
             'm1_visibility_docs', 'public.m1_visibility_docs'
         );
         SELECT pgcontext.upsert_points(
             'm1_visibility_docs', ARRAY['tenant-secret-source-key']
         );",
    )
    .expect("collection owner should seed a private source key");
    acl_reset_session_user();

    Spi::run(
        r#"
        CREATE FUNCTION public.m1_hostile_source_key_predicate(value text)
        RETURNS boolean
        LANGUAGE plpgsql
        IMMUTABLE
        STRICT
        COST 0.0001
        AS $$
        BEGIN
            RAISE EXCEPTION 'hostile predicate observed source_key: %', value;
        END;
        $$;
        GRANT EXECUTE ON FUNCTION public.m1_hostile_source_key_predicate(text)
            TO m1_visibility_outsider;
        "#,
    )
    .expect("hostile non-leakproof predicate should be created");

    acl_set_session_user("m1_visibility_outsider");
    Spi::run(
        r#"
        CREATE TEMP TABLE m1_visibility_probe_result (observed boolean NOT NULL);
        DO $$
        BEGIN
            BEGIN
                PERFORM 1
                  FROM pgcontext._visible_collection_points
                 WHERE public.m1_hostile_source_key_predicate(source_key);
                INSERT INTO m1_visibility_probe_result VALUES (false);
            EXCEPTION WHEN raise_exception THEN
                INSERT INTO m1_visibility_probe_result VALUES (true);
            END;
        END;
        $$;
        "#,
    )
    .expect("outsider visibility probe should execute");
    let observed = Spi::get_one::<bool>("SELECT bool_or(observed) FROM m1_visibility_probe_result")
        .expect("probe result query should execute")
        .expect("probe result should not be null");
    assert!(
        !observed,
        "a non-leakproof predicate must not execute on another tenant's source_key"
    );
    acl_reset_session_user();
}

#[pg_test]
fn automatic_query_stats_are_written_and_visible_through_membership_views() {
    acl_create_role("m15_stats_owner");
    acl_create_role("m15_stats_member");
    acl_create_role("m15_stats_outsider");
    acl_grant_api_access("m15_stats_owner");
    acl_grant_api_access("m15_stats_member");
    acl_grant_api_access("m15_stats_outsider");
    Spi::run("GRANT m15_stats_owner TO m15_stats_member")
        .expect("member should receive the collection owner role");

    acl_set_session_user("m15_stats_owner");
    Spi::run(
        "CREATE TABLE public.m15_stats_docs (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL
         );
         INSERT INTO public.m15_stats_docs VALUES
             (1, '[1,0]'::vector),
             (2, '[0,1]'::vector);
         SELECT pgcontext.create_collection(
             'm15_stats_docs', 'public.m15_stats_docs'
         );
         SELECT pgcontext.register_vector(
             'm15_stats_docs', 'embedding', 'embedding', 2, 'l2'
         );
         SELECT pgcontext.backfill_points('m15_stats_docs', 100);
         CREATE INDEX m15_stats_docs_hnsw
             ON public.m15_stats_docs
             USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ops);
         SELECT pgcontext.attach_hnsw_index(
             'm15_stats_docs', 'embedding', 'public.m15_stats_docs_hnsw'
         );",
    )
    .expect("member-visible collection should be created");
    acl_reset_session_user();

    acl_set_session_user("m15_stats_member");
    assert!(
        !acl_has_table_privilege("pgcontext._query_stats", "SELECT"),
        "collection members must not need direct _query_stats SELECT"
    );
    let result_count = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM pgcontext.execute_query(
               'm15_stats_docs',
               pgcontext.query_nearest('[1,0]'::vector, 1)
           )",
    )
    .expect("member query should execute")
    .expect("member query count should not be null");
    assert_eq!(result_count, 1);
    let collection_id = Spi::get_one::<i64>(
        "SELECT collection_id
           FROM pgcontext._collection_acl
          WHERE collection_name = 'm15_stats_docs'",
    )
    .expect("member collection lookup should execute")
    .expect("member collection should be visible");
    let events = crate::query_stats_async::test_events(collection_id);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].strategy, "dense_hnsw");
    assert_eq!(events[0].completion, "complete");
    acl_reset_session_user();

    // The pgrx wrapper suite runs each fixture in one uncommitted transaction,
    // so its test build captures async events backend-locally. Seed the private
    // sink as the extension owner to verify the production visibility boundary;
    // the live heavy gate verifies the worker performs this insert itself.
    Spi::run_with_args(
        "INSERT INTO pgcontext._query_stats (
             collection_id, cohort, query_kind, result_count, candidate_count,
             rows_rechecked, rows_pruned, latency_bucket, lifecycle_state,
             latency_ms, strategy, visits, filter_candidates, candidates,
             rechecks, stages, expansions, completion
         ) VALUES (
             $1, 'automatic', 'search', 1, 1, 1, 0, 'Lt1Ms', 'Indexed',
             0.1, 'dense_hnsw', 2, 0, 1, 1, 2, 1, 'complete'
         )",
        &[collection_id.into()],
    )
    .expect("extension owner should seed the visibility fixture");

    acl_set_session_user("m15_stats_member");
    let visible_count = Spi::get_one::<i64>(
        "SELECT COALESCE(sum(query_count), 0)::bigint
           FROM pgcontext.query_execution_stats()
          WHERE collection_name = 'm15_stats_docs'",
    )
    .expect("member telemetry query should execute")
    .expect("member telemetry count should not be null");
    assert_eq!(visible_count, 1);
    acl_reset_session_user();

    acl_set_session_user("m15_stats_outsider");
    let outsider_count = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM pgcontext.query_execution_stats()
          WHERE collection_name = 'm15_stats_docs'",
    )
    .expect("outsider telemetry query should execute")
    .expect("outsider telemetry count should not be null");
    assert_eq!(outsider_count, 0);
    acl_reset_session_user();
}

#[pg_test]
fn query_telemetry_queue_health_requires_pg_monitor() {
    acl_create_role("m15_queue_health_denied");
    acl_grant_api_access("m15_queue_health_denied");
    acl_set_session_user("m15_queue_health_denied");
    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.query_telemetry_queue_stats()",
        "42501",
        "query telemetry queue health requires membership in pg_monitor",
        "automatic telemetry queue health privilege",
    );
    acl_reset_session_user();
}
