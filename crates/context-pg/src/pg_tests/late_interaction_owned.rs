#[pg_test]
fn owned_late_interaction_catalog_has_private_storage_and_public_visibility_views() {
    let table_names = Spi::get_one::<Vec<String>>(
        "SELECT pg_catalog.array_agg(class.relname::text ORDER BY class.relname)
           FROM pg_catalog.pg_class AS class
           JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
          WHERE namespace.nspname = 'pgcontext'
            AND class.relname IN (
                '_collection_late_interaction',
                '_collection_late_interaction_tokens'
            )
            AND class.relkind = 'r'",
    )
    .expect("owned late-interaction catalog query should succeed")
    .expect("owned late-interaction catalog tables should exist");
    assert_eq!(
        table_names,
        vec![
            "_collection_late_interaction".to_owned(),
            "_collection_late_interaction_tokens".to_owned(),
        ]
    );

    let visible_to_public = Spi::get_one::<bool>(
        "SELECT bool_and(pg_catalog.has_table_privilege('public', class.oid, 'SELECT'))
           FROM pg_catalog.pg_class AS class
           JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
          WHERE namespace.nspname = 'pgcontext'
            AND class.relname IN (
                '_visible_collection_late_interaction'
            )
            AND class.relkind = 'v'",
    )
    .expect("owned late-interaction visibility query should succeed")
    .expect("owned late-interaction visibility views should exist");
    assert!(visible_to_public);

    let token_visibility_view_exists = Spi::get_one::<bool>(
        "SELECT pg_catalog.to_regclass(
             'pgcontext._visible_collection_late_interaction_tokens'
         ) IS NOT NULL",
    )
    .expect("owned token visibility-view query should succeed")
    .expect("owned token visibility-view existence should not be null");
    assert!(!token_visibility_view_exists);

    let private_storage_visible_to_public = Spi::get_one::<bool>(
        "SELECT bool_or(pg_catalog.has_table_privilege('public', class.oid, 'SELECT'))
           FROM pg_catalog.pg_class AS class
           JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
          WHERE namespace.nspname = 'pgcontext'
            AND class.relname IN (
                '_collection_late_interaction',
                '_collection_late_interaction_tokens'
            )
            AND class.relkind = 'r'",
    )
    .expect("owned late-interaction private storage query should succeed")
    .expect("owned late-interaction private tables should exist");
    assert!(!private_storage_visible_to_public);
}

#[pg_test]
fn owned_late_interaction_catalog_is_extension_configuration_data() {
    let dumped_relations = Spi::get_one::<Vec<String>>(
        "SELECT pg_catalog.array_agg(class.oid::regclass::text ORDER BY class.relname)
           FROM pg_catalog.pg_extension AS extension
           JOIN unnest(extension.extconfig) AS configured(oid) ON true
           JOIN pg_catalog.pg_class AS class ON class.oid = configured.oid
          WHERE extension.extname = 'pgcontext'
            AND class.relname IN (
                '_collection_late_interaction',
                '_collection_late_interaction_tokens'
            )",
    )
    .expect("extension configuration query should succeed")
    .expect("owned late-interaction catalog should be configuration data");
    assert_eq!(
        dumped_relations,
        vec![
            "pgcontext._collection_late_interaction".to_owned(),
            "pgcontext._collection_late_interaction_tokens".to_owned(),
        ]
    );
}

#[pg_test]
fn owned_late_interaction_tokens_enforce_point_and_ordinal_identity() {
    Spi::run(
        "CREATE TABLE public.m14_owned_catalog_docs (
             id bigint PRIMARY KEY,
             token_vectors vector[] NOT NULL
         );
         INSERT INTO public.m14_owned_catalog_docs
         VALUES (1, ARRAY['[1,0]'::vector]);
         SELECT pgcontext.create_collection(
             'm14_owned_catalog_docs',
             'public.m14_owned_catalog_docs'
         );
         SELECT pgcontext.upsert_points('m14_owned_catalog_docs', ARRAY['1']);",
    )
    .expect("owned token catalog fixture should be created");

    Spi::run(
        "INSERT INTO pgcontext._collection_late_interaction_tokens (
             collection_id,
             point_id,
             token_ordinal,
             token_vector
         )
         SELECT collections.collection_id,
                points.point_id,
                1,
                '[1,0]'::vector
           FROM pgcontext._collections AS collections
           JOIN pgcontext._collection_points AS points USING (collection_id)
          WHERE collections.collection_name = 'm14_owned_catalog_docs'",
    )
    .expect("owned token row should satisfy its composite point identity");

    let ordinal = Spi::get_one::<i32>(
        "SELECT token_ordinal
           FROM pgcontext._collection_late_interaction_tokens AS tokens
           JOIN pgcontext._collections AS collections USING (collection_id)
          WHERE collections.collection_name = 'm14_owned_catalog_docs'",
    )
    .expect("owned token ordinal query should succeed")
    .expect("owned token row should exist");
    assert_eq!(ordinal, 1);
}

#[pg_test]
fn register_late_interaction_materializes_owned_tokens_and_builds_hnsw() {
    create_owned_late_interaction_fixture("m14_owned_register");

    let summary = Spi::get_one::<String>(
        "SELECT pg_catalog.concat_ws(
             '|',
             collection,
             source_table,
             token_source,
             dimensions::text,
             point_count::text,
             token_count::text,
             status
         )
           FROM pgcontext.register_late_interaction(
               'm14_owned_register',
               'public.m14_owned_register',
               'token_vectors'
           )",
    )
    .expect("owned late-interaction registration should succeed")
    .expect("owned late-interaction registration should return a summary");
    assert_eq!(
        summary,
        "m14_owned_register|public.m14_owned_register|token_vectors|2|2|4|ready"
    );

    let catalog = Spi::get_one::<String>(
        "SELECT pg_catalog.concat_ws(
             '|',
             registrations.dimensions::text,
             registrations.status,
             access_method.amname,
             registrations.point_count::text,
             registrations.token_count::text,
             pg_catalog.count(tokens.token_id)::text
         )
           FROM pgcontext._collection_late_interaction AS registrations
           JOIN pg_catalog.pg_class AS index_class
             ON index_class.oid = registrations.hnsw_index_oid
           JOIN pg_catalog.pg_am AS access_method
             ON access_method.oid = index_class.relam
           LEFT JOIN pgcontext._collection_late_interaction_tokens AS tokens
             USING (collection_id)
          GROUP BY registrations.dimensions,
                   registrations.status,
                   access_method.amname,
                   registrations.point_count,
                   registrations.token_count",
    )
    .expect("owned late-interaction catalog query should succeed")
    .expect("owned late-interaction registration should exist");
    assert_eq!(catalog, "2|ready|pgcontext_hnsw|2|4|4");

    let source_trigger_exists = Spi::get_one::<bool>(
        "SELECT EXISTS (
             SELECT 1
               FROM pg_catalog.pg_trigger
              WHERE tgrelid = 'public.m14_owned_register'::regclass
                AND tgname LIKE 'pgcontext_late_interaction_%'
                AND NOT tgisinternal
         )",
    )
    .expect("owned late-interaction trigger query should succeed")
    .expect("owned late-interaction trigger existence should not be null");
    assert!(source_trigger_exists);
}

#[pg_test]
fn owned_late_interaction_source_dml_updates_tokens_in_the_same_transaction() {
    create_owned_late_interaction_fixture("m14_owned_dml");
    register_owned_late_interaction("m14_owned_dml");

    Spi::run(
        "INSERT INTO public.m14_owned_dml
         VALUES (3, ARRAY['[0.25,0.75]'::vector, '[0.75,0.25]'::vector]);",
    )
    .expect("source insert should synchronously capture owned tokens");
    assert_eq!(owned_token_count("m14_owned_dml", "3"), 2);
    assert_eq!(owned_registration_counts("m14_owned_dml"), (3, 6));

    Spi::run(
        "UPDATE public.m14_owned_dml
            SET token_vectors = ARRAY['[1,1]'::vector]
          WHERE id = 3",
    )
    .expect("source update should synchronously replace owned tokens");
    assert_eq!(owned_token_count("m14_owned_dml", "3"), 1);
    assert_eq!(owned_registration_counts("m14_owned_dml"), (3, 5));
    let updated_vector = Spi::get_one::<String>(
        "SELECT tokens.token_vector::text
           FROM pgcontext._collection_late_interaction_tokens AS tokens
           JOIN pgcontext._collection_points AS points
             USING (collection_id, point_id)
           JOIN pgcontext._collections AS collections USING (collection_id)
          WHERE collections.collection_name = 'm14_owned_dml'
            AND points.source_key = '3'",
    )
    .expect("updated owned token query should succeed")
    .expect("updated owned token should exist");
    assert_eq!(updated_vector, "[1,1]");

    Spi::run(
        "DO $$
         BEGIN
             BEGIN
                 UPDATE public.m14_owned_dml
                    SET token_vectors = ARRAY['[0,1]'::vector, '[1,0]'::vector]
                  WHERE id = 3;
                 RAISE EXCEPTION 'force rollback';
             EXCEPTION WHEN others THEN
                 NULL;
             END;
         END $$;",
    )
    .expect("source update savepoint should roll back cleanly");
    assert_eq!(owned_token_count("m14_owned_dml", "3"), 1);
    assert_eq!(owned_registration_counts("m14_owned_dml"), (3, 5));

    Spi::run("DELETE FROM public.m14_owned_dml WHERE id = 3")
        .expect("source delete should synchronously remove owned tokens");
    assert_eq!(owned_token_count("m14_owned_dml", "3"), 0);
    assert_eq!(owned_registration_counts("m14_owned_dml"), (2, 4));
    let deleted = Spi::get_one::<bool>(
        "SELECT points.deleted_at IS NOT NULL
           FROM pgcontext._collection_points AS points
           JOIN pgcontext._collections AS collections USING (collection_id)
          WHERE collections.collection_name = 'm14_owned_dml'
            AND points.source_key = '3'",
    )
    .expect("deleted point query should succeed")
    .expect("deleted point mapping should remain");
    assert!(deleted);
}

#[pg_test]
fn owned_late_interaction_soft_delete_and_reactivation_preserve_derived_tokens() {
    create_owned_late_interaction_fixture("m14_owned_reactivate");
    register_owned_late_interaction("m14_owned_reactivate");

    Spi::run("SELECT pgcontext.delete_points('m14_owned_reactivate', ARRAY['1'])")
        .expect("owned late-interaction point should be soft deleted");
    assert_eq!(owned_token_count("m14_owned_reactivate", "1"), 2);
    assert_eq!(owned_registration_counts("m14_owned_reactivate"), (2, 4));
    let deleted_rows = hybrid_query_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_late_interaction_ann(
               'm14_owned_reactivate',
               ARRAY['[1,0]'::vector],
               4,
               2
           )",
    );
    assert!(deleted_rows.iter().all(|row| row.1 != "1"));

    Spi::run("SELECT pgcontext.repair_late_interaction('m14_owned_reactivate', 1)")
        .expect("owned late-interaction repair should preserve point tombstones");
    let remains_deleted = Spi::get_one::<bool>(
        "SELECT points.deleted_at IS NOT NULL
           FROM pgcontext._collection_points AS points
           JOIN pgcontext._collections AS collections USING (collection_id)
          WHERE collections.collection_name = 'm14_owned_reactivate'
            AND points.source_key = '1'",
    )
    .expect("repaired owned point tombstone query should succeed")
    .expect("repaired owned point tombstone should exist");
    assert!(remains_deleted);

    Spi::run("SELECT pgcontext.upsert_points('m14_owned_reactivate', ARRAY['1'])")
        .expect("owned late-interaction point should be reactivated");
    let reactivated_rows = hybrid_query_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_late_interaction_ann(
               'm14_owned_reactivate',
               ARRAY['[1,0]'::vector],
               4,
               2
           )",
    );
    assert!(reactivated_rows.iter().any(|row| row.1 == "1"));
    assert_eq!(owned_token_count("m14_owned_reactivate", "1"), 2);
    assert_eq!(owned_registration_counts("m14_owned_reactivate"), (2, 4));
}

#[pg_test]
fn owned_late_interaction_internal_store_cannot_accept_caller_token_payloads() {
    create_owned_late_interaction_fixture("m14_owned_store_boundary");
    register_owned_late_interaction("m14_owned_store_boundary");
    let collection_id = owned_collection_id("m14_owned_store_boundary");
    let payload_overload_exists = Spi::get_one::<bool>(
        "SELECT pg_catalog.to_regprocedure(
             'pgcontext._store_late_interaction_tokens(bigint,text,vector[])'
         ) IS NOT NULL",
    )
    .expect("owned store payload-overload query should succeed")
    .expect("owned store payload-overload existence should not be null");
    assert!(!payload_overload_exists);

    shared_assert_sql_failure(
        &format!(
            "SELECT pgcontext._store_late_interaction_tokens({collection_id}, 'missing')"
        ),
        "42704",
        "late-interaction source row does not exist for source key missing",
        "owned late-interaction store source validation",
    );
    assert_eq!(owned_registration_counts("m14_owned_store_boundary"), (2, 4));
}

#[pg_test]
fn owned_late_interaction_invalid_source_update_rolls_back_every_derived_write() {
    create_owned_late_interaction_fixture("m14_owned_invalid_rollback");
    register_owned_late_interaction("m14_owned_invalid_rollback");
    Spi::run(
        "DO $$
         DECLARE
             actual_sqlstate text;
             actual_message text;
         BEGIN
             BEGIN
                 UPDATE public.m14_owned_invalid_rollback
                    SET token_vectors = ARRAY[]::vector[]
                  WHERE id = 1;
                 RAISE EXCEPTION 'expected invalid source update to fail';
             EXCEPTION WHEN OTHERS THEN
                 GET STACKED DIAGNOSTICS
                     actual_sqlstate = RETURNED_SQLSTATE,
                     actual_message = MESSAGE_TEXT;
                 IF actual_sqlstate <> '22023'
                    OR actual_message <> 'late-interaction token source must contain at least one non-null vector for source key 1' THEN
                     RAISE EXCEPTION 'unexpected invalid source update error: % %',
                         actual_sqlstate,
                         actual_message;
                 END IF;
             END;
         END
         $$;",
    )
    .expect("invalid owned late-interaction source update should roll back");
    let source_token_count = Spi::get_one::<i32>(
        "SELECT pg_catalog.cardinality(token_vectors)
           FROM public.m14_owned_invalid_rollback
          WHERE id = 1",
    )
    .expect("rolled-back source token count query should succeed")
    .expect("rolled-back source token count should not be null");
    assert_eq!(source_token_count, 2);
    assert_eq!(owned_token_count("m14_owned_invalid_rollback", "1"), 2);
    assert_eq!(
        owned_registration_counts("m14_owned_invalid_rollback"),
        (2, 4)
    );
}

#[pg_test]
fn owned_late_interaction_ann_rejects_token_source_schema_drift() {
    create_owned_late_interaction_fixture("m14_owned_schema_drift");
    register_owned_late_interaction("m14_owned_schema_drift");
    Spi::run(
        "ALTER TABLE public.m14_owned_schema_drift
         DROP COLUMN token_vectors CASCADE",
    )
    .expect("owned late-interaction token source should be dropped");
    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction_ann(
             'm14_owned_schema_drift',
             ARRAY['[1,0]'::vector],
             2,
             2
         )",
        "55000",
        "late-interaction source binding has drifted; run pgcontext.repair_late_interaction",
        "owned late-interaction token source drift",
    );
}

#[pg_test]
fn owned_late_interaction_ann_search_uses_registration_and_exact_reranks() {
    create_owned_late_interaction_fixture("m14_owned_search");
    register_owned_late_interaction("m14_owned_search");

    let rows = hybrid_query_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_late_interaction_ann(
               'm14_owned_search',
               ARRAY['[1,0]'::vector, '[0,1]'::vector],
               2,
               2
           )",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, "1");
    assert!((rows[0].2 - 2.0).abs() < 1e-6);
    assert_eq!(rows[1].1, "2");
    assert!((rows[1].2 - 1.8).abs() < 1e-6);

    let explain = Spi::get_one::<String>(
        "SELECT pg_catalog.string_agg(stage || '|' || detail || '|' || strategy, E'\\n')
           FROM pgcontext.explain_late_interaction_ann(
               'm14_owned_search',
               ARRAY['[1,0]'::vector, '[0,1]'::vector],
               2
           )",
    )
    .expect("owned late-interaction explain should succeed")
    .expect("owned late-interaction explain should return rows");
    assert!(explain.contains("owned_relation=pgcontext._collection_late_interaction_tokens"));
    assert!(!explain.contains("points="));
    assert!(!explain.contains("tokens="));
    assert!(explain.contains("owned_hnsw_token_candidates"));
}

#[pg_test]
fn owned_late_interaction_ann_matches_exact_maxsim_when_candidate_budget_covers_tokens() {
    Spi::run(
        "CREATE TABLE public.m14_owned_exact_oracle (
             id bigint PRIMARY KEY,
             token_vectors vector[] NOT NULL
         );
         INSERT INTO public.m14_owned_exact_oracle
         SELECT value,
                ARRAY[
                    ARRAY[(value::real / 16::real)::real, 1::real]::vector,
                    ARRAY[1::real, (value::real / 16::real)::real]::vector,
                    ARRAY[(value % 3)::real, (value % 5)::real]::vector
                ]
           FROM pg_catalog.generate_series(1, 12) AS value;
         SELECT pgcontext.create_collection(
             'm14_owned_exact_oracle',
             'public.m14_owned_exact_oracle'
         );
         SELECT pgcontext.register_late_interaction(
             'm14_owned_exact_oracle',
             'public.m14_owned_exact_oracle',
             'token_vectors'
         );",
    )
    .expect("owned late-interaction exact-oracle fixture should be created");
    let rankings_match = Spi::get_one::<bool>(
        "WITH exact AS (
             SELECT pg_catalog.array_agg(source_key ORDER BY score DESC, point_id) AS keys
               FROM pgcontext.search_late_interaction(
                   'm14_owned_exact_oracle',
                   ARRAY['[1,0]'::vector, '[0,1]'::vector],
                   'token_vectors',
                   12
               )
         ), approximate AS (
             SELECT pg_catalog.array_agg(source_key ORDER BY score DESC, point_id) AS keys
               FROM pgcontext.search_late_interaction_ann(
                   'm14_owned_exact_oracle',
                   ARRAY['[1,0]'::vector, '[0,1]'::vector],
                   36,
                   12
               )
         )
         SELECT exact.keys = approximate.keys
           FROM exact CROSS JOIN approximate",
    )
    .expect("owned late-interaction oracle comparison should succeed")
    .expect("owned late-interaction oracle comparison should not be null");
    assert!(rankings_match);
}

#[pg_test]
fn owned_late_interaction_candidate_query_can_use_collection_hnsw_index() {
    Spi::run(
        "CREATE TABLE public.m14_owned_index_plan (
             id bigint PRIMARY KEY,
             token_vectors vector[] NOT NULL
         );
         INSERT INTO public.m14_owned_index_plan
         SELECT value,
                ARRAY[
                    ARRAY[value::real, 1::real]::vector,
                    ARRAY[1::real, value::real]::vector
                ]
           FROM pg_catalog.generate_series(1, 128) AS value;
         SELECT pgcontext.create_collection(
             'm14_owned_index_plan',
             'public.m14_owned_index_plan'
         );",
    )
    .expect("owned late-interaction candidate path fixture should be created");
    register_owned_late_interaction("m14_owned_index_plan");
    let collection_id = owned_collection_id("m14_owned_index_plan");
    let candidate_count = Spi::get_one::<i64>(&format!(
        "SELECT pg_catalog.count(*)::bigint
           FROM pgcontext._late_interaction_ann_candidate_points(
               {collection_id},
               '[1,0]'::vector,
               8
           )"
    ))
    .expect("owned late-interaction candidate helper should succeed")
    .expect("owned late-interaction candidate count should not be null");
    assert_eq!(candidate_count, 8);
    let scan_work = Spi::get_one::<String>(
        "SELECT page_visits::text || '|' || exact_strategy::text
           FROM pgcontext.hnsw_last_scan_work()",
    )
    .expect("owned late-interaction HNSW scan work should be readable")
    .expect("owned late-interaction HNSW scan work should exist");
    let (page_visits, exact_strategy) = scan_work
        .split_once('|')
        .expect("owned late-interaction HNSW scan work should be structured");
    assert!(
        page_visits
            .parse::<i64>()
            .expect("owned late-interaction page visits should be numeric")
            > 0
    );
    assert_eq!(exact_strategy, "false");
}

#[pg_test]
fn empty_owned_late_interaction_ann_search_is_an_exact_noop() {
    Spi::run(
        "CREATE TABLE public.m14_owned_empty_search (
             id bigint PRIMARY KEY,
             token_vectors vector[] NOT NULL
         );
         SELECT pgcontext.create_collection(
             'm14_owned_empty_search',
             'public.m14_owned_empty_search'
         );
         SELECT pgcontext.register_late_interaction(
             'm14_owned_empty_search',
             'public.m14_owned_empty_search',
             'token_vectors'
         );",
    )
    .expect("empty owned late-interaction search fixture should be created");

    let count = Spi::get_one::<i64>(
        "SELECT pg_catalog.count(*)::bigint
           FROM pgcontext.search_late_interaction_ann(
               'm14_owned_empty_search',
               ARRAY['[1,0]'::vector],
               10,
               10
           )",
    )
    .expect("empty owned late-interaction search should succeed")
    .expect("empty owned late-interaction count should not be null");
    assert_eq!(count, 0);
}

#[pg_test]
fn owned_late_interaction_ann_expands_past_rls_hidden_neighbors() {
    sql_test_create_role("m14_owned_rls_owner");
    sql_test_create_role("m14_owned_rls_caller");
    Spi::run(
        "GRANT m14_owned_rls_owner TO m14_owned_rls_caller;
         GRANT USAGE ON SCHEMA public, pgcontext
             TO m14_owned_rls_owner, m14_owned_rls_caller;
         GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA pgcontext
             TO m14_owned_rls_owner, m14_owned_rls_caller;
         CREATE TABLE public.m14_owned_rls_docs (
             id bigint PRIMARY KEY,
             tenant_name text NOT NULL,
             token_vectors vector[] NOT NULL
         );
         ALTER TABLE public.m14_owned_rls_docs OWNER TO m14_owned_rls_owner;
         ALTER TABLE public.m14_owned_rls_docs ENABLE ROW LEVEL SECURITY;
         ALTER TABLE public.m14_owned_rls_docs FORCE ROW LEVEL SECURITY;
         CREATE POLICY m14_owned_rls_policy ON public.m14_owned_rls_docs
             USING (
                 tenant_name = SESSION_USER
                 OR SESSION_USER = 'm14_owned_rls_owner'
             )
             WITH CHECK (
                 tenant_name = SESSION_USER
                 OR SESSION_USER = 'm14_owned_rls_owner'
             );
         GRANT SELECT ON public.m14_owned_rls_docs TO m14_owned_rls_caller;
         INSERT INTO public.m14_owned_rls_docs
         VALUES
             (1, 'hidden', ARRAY['[10,0]'::vector]),
             (2, 'm14_owned_rls_caller', ARRAY['[1,0]'::vector]);",
    )
    .expect("owned late-interaction RLS fixture should be created");

    sql_test_set_session_user("m14_owned_rls_owner");
    Spi::run(
        "SELECT pgcontext.create_collection(
             'm14_owned_rls_docs',
             'public.m14_owned_rls_docs'
         );
         SELECT pgcontext.register_late_interaction(
             'm14_owned_rls_docs',
             'public.m14_owned_rls_docs',
             'token_vectors'
         );",
    )
    .expect("owned late-interaction RLS collection should be registered");
    sql_test_reset_session_user();

    sql_test_set_session_user("m14_owned_rls_caller");
    let visible_key = Spi::get_one::<String>(
        "SELECT source_key
           FROM pgcontext.search_late_interaction_ann(
               'm14_owned_rls_docs',
               ARRAY['[1,0]'::vector],
               1,
               1
           )",
    )
    .expect("owned late-interaction RLS search should succeed")
    .expect("owned late-interaction RLS search should refill a visible row");
    sql_test_reset_session_user();
    assert_eq!(visible_key, "2");
}

#[pg_test]
fn owned_late_interaction_ann_search_rejects_a_missing_generation() {
    create_owned_late_interaction_fixture("m14_owned_missing_generation");
    register_owned_late_interaction("m14_owned_missing_generation");
    let collection_id = owned_collection_id("m14_owned_missing_generation");
    Spi::run(&format!(
        "DROP INDEX pgcontext.pgcontext_late_interaction_{collection_id}_hnsw"
    ))
    .expect("owned late-interaction generation should be removed");

    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction_ann(
             'm14_owned_missing_generation',
             ARRAY['[1,0]'::vector],
             2,
             2
         )",
        "55000",
        "late-interaction ANN generation is not ready; run pgcontext.repair_late_interaction",
        "owned late-interaction missing generation",
    );
}

#[pg_test]
fn owned_late_interaction_ann_rejects_a_misbound_hnsw_generation() {
    create_owned_late_interaction_fixture("m14_owned_misbound_generation");
    register_owned_late_interaction("m14_owned_misbound_generation");
    let collection_id = owned_collection_id("m14_owned_misbound_generation");
    Spi::run(&format!(
        "CREATE INDEX m14_owned_wrong_generation
             ON pgcontext._collection_late_interaction_tokens
             USING pgcontext_hnsw (
                 (token_vector::public.vector(2)) pgcontext.vector_hnsw_ip_ops
             )
             WHERE collection_id = {};
         UPDATE pgcontext._collection_late_interaction
            SET hnsw_index_oid = 'pgcontext.m14_owned_wrong_generation'::regclass
          WHERE collection_id = {collection_id}",
        collection_id + 1
    ))
    .expect("owned late-interaction registration should be misbound for testing");

    shared_assert_sql_failure(
        "SELECT pgcontext.search_late_interaction_ann(
             'm14_owned_misbound_generation',
             ARRAY['[1,0]'::vector],
             2,
             2
         )",
        "55000",
        "late-interaction ANN generation is not ready; run pgcontext.repair_late_interaction",
        "owned late-interaction misbound generation",
    );
}

#[pg_test]
fn dropping_owned_late_interaction_collection_removes_dynamic_objects() {
    create_owned_late_interaction_fixture("m14_owned_cleanup");
    register_owned_late_interaction("m14_owned_cleanup");
    let index_name = Spi::get_one::<String>(
        "SELECT index_class.oid::regclass::text
           FROM pgcontext._collection_late_interaction AS registrations
           JOIN pg_catalog.pg_class AS index_class
             ON index_class.oid = registrations.hnsw_index_oid",
    )
    .expect("owned late-interaction index query should succeed")
    .expect("owned late-interaction index should exist");

    Spi::run("SELECT pgcontext.drop_collection('m14_owned_cleanup')")
        .expect("owned late-interaction collection drop should succeed");

    let index_exists = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.to_regclass($1) IS NOT NULL",
        &[index_name.as_str().into()],
    )
    .expect("dropped owned index query should succeed")
    .expect("dropped owned index existence should not be null");
    assert!(!index_exists);
    let source_trigger_exists = Spi::get_one::<bool>(
        "SELECT EXISTS (
             SELECT 1
               FROM pg_catalog.pg_trigger
              WHERE tgrelid = 'public.m14_owned_cleanup'::regclass
                AND tgname LIKE 'pgcontext_late_interaction_%'
                AND NOT tgisinternal
         )",
    )
    .expect("dropped source trigger query should succeed")
    .expect("dropped source trigger existence should not be null");
    assert!(!source_trigger_exists);
}

#[pg_test]
fn dropping_collection_removes_owned_trigger_after_source_rename() {
    create_owned_late_interaction_fixture("m14_owned_rename_cleanup");
    register_owned_late_interaction("m14_owned_rename_cleanup");
    Spi::run(
        "ALTER TABLE public.m14_owned_rename_cleanup
         RENAME TO m14_owned_rename_cleanup_moved",
    )
    .expect("owned late-interaction source should be renamed");
    Spi::run("SELECT pgcontext.drop_collection('m14_owned_rename_cleanup')")
        .expect("renamed owned late-interaction collection should be dropped");
    let trigger_exists = Spi::get_one::<bool>(
        "SELECT EXISTS (
             SELECT 1
               FROM pg_catalog.pg_trigger
              WHERE tgrelid = 'public.m14_owned_rename_cleanup_moved'::regclass
                AND tgname LIKE 'pgcontext_late_interaction_%'
                AND NOT tgisinternal
         )",
    )
    .expect("renamed owned source trigger query should succeed")
    .expect("renamed owned source trigger existence should not be null");
    assert!(!trigger_exists);
}

#[pg_test]
fn register_late_interaction_rejects_partitioned_sources_explicitly() {
    Spi::run(
        "CREATE TABLE public.m14_owned_partitioned (
             id bigint PRIMARY KEY,
             token_vectors vector[] NOT NULL
         ) PARTITION BY RANGE (id);
         CREATE TABLE public.m14_owned_partitioned_p1
             PARTITION OF public.m14_owned_partitioned FOR VALUES FROM (0) TO (10);
         INSERT INTO public.m14_owned_partitioned
         VALUES (1, ARRAY['[1,0]'::vector]);
         SELECT pgcontext.create_collection(
             'm14_owned_partitioned',
             'public.m14_owned_partitioned'
         );",
    )
    .expect("partitioned owned late-interaction fixture should be created");
    shared_assert_sql_failure(
        "SELECT pgcontext.register_late_interaction(
             'm14_owned_partitioned',
             'public.m14_owned_partitioned',
             'token_vectors'
         )",
        "0A000",
        "late-interaction registration requires an ordinary table source; partitioned tables are not supported",
        "owned late-interaction partitioned source rejection",
    );
}

#[pg_test]
fn register_late_interaction_requires_a_not_null_unique_source_key() {
    Spi::run(
        "CREATE TABLE public.m14_owned_nonunique_id (
             id bigint,
             token_vectors vector[] NOT NULL
         );
         INSERT INTO public.m14_owned_nonunique_id
         VALUES (1, ARRAY['[1,0]'::vector]),
                (1, ARRAY['[0,1]'::vector]);
         SELECT pgcontext.create_collection(
             'm14_owned_nonunique_id',
             'public.m14_owned_nonunique_id'
         );",
    )
    .expect("non-unique owned late-interaction fixture should be created");
    shared_assert_sql_failure(
        "SELECT pgcontext.register_late_interaction(
             'm14_owned_nonunique_id',
             'public.m14_owned_nonunique_id',
             'token_vectors'
         )",
        "55000",
        "late-interaction source key must be a NOT NULL single-column immediate unique key: public.m14_owned_nonunique_id.id",
        "owned late-interaction source identity",
    );
}

#[pg_test]
fn repair_late_interaction_rebuilds_tokens_and_hnsw_in_bounded_batches() {
    create_owned_late_interaction_fixture("m14_owned_repair");
    register_owned_late_interaction("m14_owned_repair");
    let previous_index = owned_late_interaction_index_oid("m14_owned_repair");
    Spi::run(
        "DELETE FROM pgcontext._collection_late_interaction_tokens AS tokens
          USING pgcontext._collection_points AS points,
                pgcontext._collections AS collections
          WHERE tokens.collection_id = points.collection_id
            AND tokens.point_id = points.point_id
            AND points.collection_id = collections.collection_id
            AND collections.collection_name = 'm14_owned_repair'
            AND points.source_key = '1'",
    )
    .expect("owned token corruption fixture should be created");
    assert_eq!(owned_token_count("m14_owned_repair", "1"), 0);

    let summary = Spi::get_one::<String>(
        "SELECT pg_catalog.concat_ws(
             '|',
             collection,
             batch_count::text,
             point_count::text,
             token_count::text,
             dimensions::text,
             status
         )
           FROM pgcontext.repair_late_interaction('m14_owned_repair', 1)",
    )
    .expect("owned late-interaction repair should succeed")
    .expect("owned late-interaction repair should return a summary");
    assert_eq!(summary, "m14_owned_repair|2|2|4|2|ready");
    assert_eq!(owned_token_count("m14_owned_repair", "1"), 2);
    let rebuilt_index = owned_late_interaction_index_oid("m14_owned_repair");
    assert_ne!(rebuilt_index, previous_index);
}

#[pg_test]
fn repair_late_interaction_promotes_an_empty_registration_after_source_insert() {
    Spi::run(
        "CREATE TABLE public.m14_owned_empty_repair (
             id bigint PRIMARY KEY,
             token_vectors vector[] NOT NULL
         );
         SELECT pgcontext.create_collection(
             'm14_owned_empty_repair',
             'public.m14_owned_empty_repair'
         );",
    )
    .expect("empty owned late-interaction fixture should be created");
    let initial_status = Spi::get_one::<String>(
        "SELECT status
           FROM pgcontext.register_late_interaction(
               'm14_owned_empty_repair',
               'public.m14_owned_empty_repair',
               'token_vectors'
           )",
    )
    .expect("empty owned late-interaction registration should succeed")
    .expect("empty owned late-interaction registration should return status");
    assert_eq!(initial_status, "building");

    Spi::run(
        "INSERT INTO public.m14_owned_empty_repair
         VALUES (1, ARRAY['[1,0]'::vector, '[0,1]'::vector])",
    )
    .expect("first source insert should be synchronously captured");
    let building_state = Spi::get_one::<String>(
        "SELECT pg_catalog.concat_ws('|', status, dimensions::text, hnsw_index_oid::text)
           FROM pgcontext._collection_late_interaction AS registrations
           JOIN pgcontext._collections AS collections USING (collection_id)
          WHERE collections.collection_name = 'm14_owned_empty_repair'",
    )
    .expect("building registration query should succeed")
    .expect("building registration should exist");
    assert_eq!(building_state, "building|2");

    let repaired_status = Spi::get_one::<String>(
        "SELECT status
           FROM pgcontext.repair_late_interaction('m14_owned_empty_repair', 10)",
    )
    .expect("empty owned late-interaction repair should succeed")
    .expect("empty owned late-interaction repair should return status");
    assert_eq!(repaired_status, "ready");
    assert_ne!(
        owned_late_interaction_index_oid("m14_owned_empty_repair"),
        pg_sys::InvalidOid
    );
}

#[pg_test]
fn rolled_back_late_interaction_repair_preserves_previous_generation() {
    create_owned_late_interaction_fixture("m14_owned_repair_rollback");
    register_owned_late_interaction("m14_owned_repair_rollback");
    Spi::run(
        "DELETE FROM pgcontext._collection_late_interaction_tokens
          WHERE token_id = (
              SELECT min(tokens.token_id)
                FROM pgcontext._collection_late_interaction_tokens AS tokens
                JOIN pgcontext._collections AS collections USING (collection_id)
               WHERE collections.collection_name = 'm14_owned_repair_rollback'
          )",
    )
    .expect("repair rollback fixture should remove one token");
    let previous_index = owned_late_interaction_index_oid("m14_owned_repair_rollback");
    let previous_token_count = owned_collection_token_count("m14_owned_repair_rollback");

    Spi::run(
        "DO $$
         BEGIN
             BEGIN
                 PERFORM pgcontext.repair_late_interaction(
                     'm14_owned_repair_rollback',
                     1
                 );
                 RAISE EXCEPTION 'force repair rollback';
             EXCEPTION WHEN others THEN
                 NULL;
             END;
         END $$;",
    )
    .expect("late-interaction repair savepoint should roll back cleanly");

    assert_eq!(
        owned_collection_token_count("m14_owned_repair_rollback"),
        previous_token_count
    );
    assert_eq!(
        owned_late_interaction_index_oid("m14_owned_repair_rollback"),
        previous_index
    );
}

fn create_owned_late_interaction_fixture(collection_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             token_vectors vector[] NOT NULL
         );
         INSERT INTO public.{collection_name}
         VALUES (1, ARRAY['[1,0]'::vector, '[0,1]'::vector]),
                (2, ARRAY['[0.9,0.1]'::vector, '[0.1,0.9]'::vector]);
         SELECT pgcontext.create_collection(
             '{collection_name}',
             'public.{collection_name}'
         );"
    ))
    .expect("owned late-interaction fixture should be created");
}

fn register_owned_late_interaction(collection_name: &str) {
    Spi::run(&format!(
        "SELECT pgcontext.register_late_interaction(
             '{collection_name}',
             'public.{collection_name}',
             'token_vectors'
         )"
    ))
    .expect("owned late-interaction fixture should be registered");
}

fn owned_token_count(collection_name: &str, source_key: &str) -> i64 {
    Spi::get_one_with_args::<i64>(
        "SELECT pg_catalog.count(*)::bigint
           FROM pgcontext._collection_late_interaction_tokens AS tokens
           JOIN pgcontext._collection_points AS points
             USING (collection_id, point_id)
           JOIN pgcontext._collections AS collections USING (collection_id)
          WHERE collections.collection_name = $1
            AND points.source_key = $2",
        &[collection_name.into(), source_key.into()],
    )
    .expect("owned token count query should succeed")
    .expect("owned token count should not be null")
}

fn owned_collection_token_count(collection_name: &str) -> i64 {
    Spi::get_one_with_args::<i64>(
        "SELECT pg_catalog.count(*)::bigint
           FROM pgcontext._collection_late_interaction_tokens AS tokens
           JOIN pgcontext._collections AS collections USING (collection_id)
          WHERE collections.collection_name = $1",
        &[collection_name.into()],
    )
    .expect("owned collection token count query should succeed")
    .expect("owned collection token count should not be null")
}

fn owned_late_interaction_index_oid(collection_name: &str) -> pg_sys::Oid {
    Spi::get_one_with_args::<pg_sys::Oid>(
        "SELECT registrations.hnsw_index_oid
           FROM pgcontext._collection_late_interaction AS registrations
           JOIN pgcontext._collections AS collections USING (collection_id)
          WHERE collections.collection_name = $1",
        &[collection_name.into()],
    )
    .expect("owned late-interaction index oid query should succeed")
    .expect("owned late-interaction index oid should not be null")
}

fn owned_collection_id(collection_name: &str) -> i64 {
    Spi::get_one_with_args::<i64>(
        "SELECT collection_id
           FROM pgcontext._collections
          WHERE collection_name = $1",
        &[collection_name.into()],
    )
    .expect("owned collection id query should succeed")
    .expect("owned collection id should not be null")
}

fn owned_registration_counts(collection_name: &str) -> (i64, i64) {
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT registrations.point_count, registrations.token_count
                   FROM pgcontext._collection_late_interaction AS registrations
                   JOIN pgcontext._collections AS collections USING (collection_id)
                  WHERE collections.collection_name = $1",
                Some(1),
                &[collection_name.into()],
            )
            .expect("owned registration count query should succeed");
        let row = rows.first();
        (
            row.get::<i64>(1)
                .expect("owned registration point count should be readable")
                .expect("owned registration point count should not be null"),
            row.get::<i64>(2)
                .expect("owned registration token count should be readable")
                .expect("owned registration token count should not be null"),
        )
    })
}
