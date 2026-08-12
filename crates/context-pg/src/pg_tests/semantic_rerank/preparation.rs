#[pg_test]
fn semantic_rerank_catalog_is_private_and_views_are_security_barriers() {
    let private_count = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM pg_catalog.pg_class AS class
           JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
          WHERE namespace.nspname = 'pgcontext'
            AND class.relkind = 'r'
            AND class.relname IN (
                '_semantic_rerank_sources',
                '_semantic_rerank_requests',
                '_semantic_rerank_candidates'
            )
            AND NOT pg_catalog.has_table_privilege('public', class.oid, 'SELECT')",
    )
    .expect("private catalog query should succeed")
    .expect("private catalog count should exist");
    assert_eq!(private_count, 3);

    let barrier_count = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM pg_catalog.pg_class AS class
           JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
          WHERE namespace.nspname = 'pgcontext'
            AND class.relkind = 'v'
            AND class.relname IN (
                '_visible_semantic_rerank_sources',
                '_visible_semantic_rerank_requests',
                '_visible_semantic_rerank_candidates'
            )
            AND class.reloptions @> ARRAY['security_barrier=true']",
    )
    .expect("barrier query should succeed")
    .expect("barrier count should exist");
    assert_eq!(barrier_count, 3);
}

#[pg_test]
fn semantic_rerank_prepare_and_finalize_recheck_authority_and_order() {
    semantic_rerank_fixture("semantic_roundtrip");
    let envelope = prepare_semantic_fixture("semantic_roundtrip", "require_reranker", false);
    assert_eq!(envelope.0["version"], 3);
    assert_eq!(envelope.0["model"], "linear-pair-v1");
    assert_eq!(envelope.0["candidates"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        envelope.0["candidates"][0]["text"],
        "postgres storage internals"
    );
    let request_id = envelope.0["request_id"].as_i64().expect("request id");
    let response = JsonB(json!({
        "version": 3,
        "request_id": request_id,
        "model": "linear-pair-v1",
        "model_revision": 7,
        "scores": [
            {"occurrence_id": 12, "score": 0.9},
            {"occurrence_id": 11, "score": 0.2}
        ]
    }));
    let result = Spi::get_one_with_args::<JsonB>(
        "SELECT pgcontext.finalize_semantic_rerank($1, $2, NULL)",
        &[request_id.into(), response.into()],
    )
    .expect("semantic rerank finalization should succeed")
    .expect("semantic rerank result should exist");
    assert_eq!(result.0["status"], "reranked");
    assert_eq!(result.0["completion"], "complete");
    assert_eq!(result.0["results"][0]["occurrence_id"], 12);
    assert!(result.0["results"][0].get("text").is_none());

    let replay = Spi::get_one_with_args::<JsonB>(
        "SELECT pgcontext.finalize_semantic_rerank($1, $2, NULL)",
        &[request_id.into(), JsonB(json!({
            "version": 3,
            "request_id": request_id,
            "model": "linear-pair-v1",
            "model_revision": 7,
            "scores": [
                {"occurrence_id": 12, "score": 0.9},
                {"occurrence_id": 11, "score": 0.2}
            ]
        })).into()],
    )
    .expect("identical replay should succeed")
    .expect("identical replay should return the stored result");
    assert_eq!(replay.0, result.0);
}

#[pg_test]
fn semantic_rerank_fallback_is_explicit_but_source_drift_is_fail_closed() {
    semantic_rerank_fixture("semantic_fallback");
    let envelope = prepare_semantic_fixture(
        "semantic_fallback",
        "allow_fused_fallback",
        false,
    );
    let request_id = envelope.0["request_id"].as_i64().expect("request id");
    let fallback = Spi::get_one_with_args::<JsonB>(
        "SELECT pgcontext.finalize_semantic_rerank($1, NULL, 'timeout')",
        &[request_id.into()],
    )
    .expect("fallback should succeed")
    .expect("fallback should return a result");
    assert_eq!(fallback.0["status"], "degraded_reranker");
    assert_eq!(fallback.0["degraded_reason"], "timeout");
    assert_eq!(fallback.0["results"][0]["occurrence_id"], 11);

    let envelope = prepare_semantic_fixture(
        "semantic_fallback",
        "allow_fused_fallback",
        false,
    );
    let request_id = envelope.0["request_id"].as_i64().expect("request id");
    Spi::run(
        "UPDATE public.semantic_fallback
            SET body = 'changed after release', source_version = source_version + 1
          WHERE id = 1",
    )
    .expect("source edit should succeed");
    shared_assert_sql_failure(
        &format!(
            "SELECT pgcontext.finalize_semantic_rerank(
                 {request_id},
                 jsonb_build_object(
                     'version', 3,
                     'request_id', {request_id},
                     'model', 'linear-pair-v1',
                     'model_revision', 7,
                     'scores', jsonb_build_array(
                         jsonb_build_object('occurrence_id', 11, 'score', 0.9),
                         jsonb_build_object('occurrence_id', 12, 'score', 0.2)
                     )
                 ),
                 NULL
             )"
        ),
        "55000",
        "semantic rerank source changed before finalization",
        "source drift after semantic rerank preparation",
    );
}

#[pg_test]
fn semantic_rerank_failure_policy_matrix_is_exact_and_partial_fallback_is_visible() {
    semantic_rerank_fixture("semantic_failure_matrix");
    for reason in [
        "unavailable",
        "timeout",
        "crash",
        "partial_output",
        "expired",
    ] {
        let envelope = prepare_semantic_fixture(
            "semantic_failure_matrix",
            "allow_fused_fallback",
            false,
        );
        let request_id = envelope.0["request_id"].as_i64().expect("request id");
        let result = Spi::get_one_with_args::<JsonB>(
            "SELECT pgcontext.finalize_semantic_rerank($1, NULL, $2)",
            &[request_id.into(), reason.into()],
        )
        .expect("allowed operational failure should degrade")
        .expect("fallback result");
        assert_eq!(result.0["status"], "degraded_reranker");
        assert_eq!(result.0["degraded_reason"], reason);

        let required = prepare_semantic_fixture(
            "semantic_failure_matrix",
            "require_reranker",
            false,
        );
        let required_id = required.0["request_id"].as_i64().expect("request id");
        shared_assert_sql_failure(
            &format!(
                "SELECT pgcontext.finalize_semantic_rerank({required_id}, NULL, '{reason}')"
            ),
            "55000",
            "semantic reranker did not produce a usable response",
            "required semantic reranker operational failure",
        );
    }

    let envelope = prepare_semantic_fixture(
        "semantic_failure_matrix",
        "allow_fused_fallback",
        false,
    );
    let request_id = envelope.0["request_id"].as_i64().expect("request id");
    let result = Spi::get_one_with_args::<JsonB>(
        "SELECT pgcontext.finalize_semantic_rerank(
             $1,
             jsonb_build_object(
                 'version', 3, 'request_id', $1,
                 'model', 'linear-pair-v1', 'model_revision', 7,
                 'scores', jsonb_build_array(
                     jsonb_build_object('occurrence_id', 11, 'score', 0.8)
                 )
             ), NULL
         )",
        &[request_id.into()],
    )
    .expect("partial operational output should degrade")
    .expect("fallback result");
    assert_eq!(result.0["status"], "degraded_reranker");
    assert_eq!(result.0["degraded_reason"], "partial_output");
    assert_eq!(result.0["results"].as_array().map(Vec::len), Some(2));
}

#[pg_test]
fn semantic_rerank_identical_replay_survives_expiry_but_new_work_does_not() {
    semantic_rerank_fixture("semantic_expiry_replay");
    let envelope = prepare_semantic_fixture("semantic_expiry_replay", "require_reranker", false);
    let request_id = envelope.0["request_id"].as_i64().expect("request id");
    let response = JsonB(json!({
        "version": 3,
        "request_id": request_id,
        "model": "linear-pair-v1",
        "model_revision": 7,
        "scores": [
            {"occurrence_id": 11, "score": 0.8},
            {"occurrence_id": 12, "score": 0.7}
        ]
    }));
    let first = Spi::get_one_with_args::<JsonB>(
        "SELECT pgcontext.finalize_semantic_rerank($1, $2, NULL)",
        &[request_id.into(), JsonB(response.0.clone()).into()],
    )
    .expect("first finalization")
    .expect("first result");
    Spi::run(&format!(
        "UPDATE pgcontext._semantic_rerank_requests
            SET expires_at_micros = 1
          WHERE request_id = {request_id}"
    ))
    .expect("test should expire finalized row");
    let replay = Spi::get_one_with_args::<JsonB>(
        "SELECT pgcontext.finalize_semantic_rerank($1, $2, NULL)",
        &[request_id.into(), response.into()],
    )
    .expect("identical post-expiry replay")
    .expect("stored replay result");
    assert_eq!(replay.0, first.0);

    let expired = prepare_semantic_fixture("semantic_expiry_replay", "require_reranker", false);
    let expired_id = expired.0["request_id"].as_i64().expect("request id");
    Spi::run(&format!(
        "UPDATE pgcontext._semantic_rerank_requests
            SET expires_at_micros = 1
          WHERE request_id = {expired_id}"
    ))
    .expect("test should expire an unfinalized row");
    shared_assert_sql_failure(
        &format!(
            "SELECT pgcontext.finalize_semantic_rerank(
                 {expired_id},
                 jsonb_build_object(
                     'version', 3, 'request_id', {expired_id},
                     'model', 'linear-pair-v1', 'model_revision', 7,
                     'scores', jsonb_build_array(
                         jsonb_build_object('occurrence_id', 11, 'score', 0.8),
                         jsonb_build_object('occurrence_id', 12, 'score', 0.7)
                     )
                 ), NULL
             )"
        ),
        "22023",
        "semantic rerank response failed validation: expired",
        "first semantic rerank finalization after expiry",
    );
}

#[pg_test]
fn semantic_rerank_source_deletion_requires_the_explicit_partial_policy() {
    semantic_rerank_fixture("semantic_delete_strict");
    let strict = prepare_semantic_fixture("semantic_delete_strict", "require_reranker", false);
    let strict_id = strict.0["request_id"].as_i64().expect("request id");
    Spi::run("DELETE FROM public.semantic_delete_strict WHERE id = 1")
        .expect("source deletion");
    shared_assert_sql_failure(
        &format!(
            "SELECT pgcontext.finalize_semantic_rerank(
                 {strict_id},
                 jsonb_build_object(
                     'version', 3, 'request_id', {strict_id},
                     'model', 'linear-pair-v1', 'model_revision', 7,
                     'scores', jsonb_build_array(
                         jsonb_build_object('occurrence_id', 11, 'score', 0.8),
                         jsonb_build_object('occurrence_id', 12, 'score', 0.7)
                     )
                 ), NULL
             )"
        ),
        "42501",
        "semantic rerank candidate visibility changed before finalization",
        "strict semantic rerank source deletion",
    );

    semantic_rerank_fixture("semantic_delete_partial");
    let partial = prepare_semantic_fixture("semantic_delete_partial", "require_reranker", true);
    let partial_id = partial.0["request_id"].as_i64().expect("request id");
    Spi::run("DELETE FROM public.semantic_delete_partial WHERE id = 1")
        .expect("source deletion");
    let result = Spi::get_one::<JsonB>(&format!(
        "SELECT pgcontext.finalize_semantic_rerank(
             {partial_id},
             jsonb_build_object(
                 'version', 3, 'request_id', {partial_id},
                 'model', 'linear-pair-v1', 'model_revision', 7,
                 'scores', jsonb_build_array(
                     jsonb_build_object('occurrence_id', 11, 'score', 0.8),
                     jsonb_build_object('occurrence_id', 12, 'score', 0.7)
                 )
             ), NULL
         )"
    ))
    .expect("partial finalization")
    .expect("partial result");
    assert_eq!(result.0["status"], "partial_reranked");
    assert_eq!(result.0["results"].as_array().map(Vec::len), Some(1));
    assert_eq!(result.0["results"][0]["occurrence_id"], 12);
}

#[pg_test]
fn semantic_rerank_raw_arguments_and_duplicate_points_fail_before_catalog_work() {
    semantic_rerank_fixture("semantic_raw_bounds");
    shared_assert_sql_failure(
        "SELECT pgcontext.prepare_semantic_rerank(
             'semantic_raw_bounds', 'body', 'postgres',
             jsonb_build_array(jsonb_build_object(
                 'occurrence_id', 1,
                 'point_id', 1,
                 'fused_rank', 1,
                 'fused_score', 1e10000::numeric,
                 'contributions', '[]'::jsonb
             )),
             'linear-pair-v1', 7, 5000, 'require_reranker', NULL, false
         )",
        "22023",
        "semantic rerank JSON numeric is outside the supported domain",
        "large semantic rerank JSON numeric",
    );
    shared_assert_sql_failure(
        "SELECT pgcontext.prepare_semantic_rerank(
             'semantic_raw_bounds', 'body', 'postgres',
             jsonb_build_array(jsonb_build_object('padding', repeat('x', 4194305))),
             'linear-pair-v1', 7, 5000, 'require_reranker', NULL, false
         )",
        "54000",
        "semantic rerank candidates exceed the JSON allocation budget",
        "oversized raw semantic rerank candidates",
    );
    shared_assert_sql_failure(
        "SELECT pgcontext.prepare_semantic_rerank(
             'semantic_raw_bounds', 'body', 'postgres',
             (SELECT jsonb_agg(NULL::text) FROM generate_series(1, 100001)),
             'linear-pair-v1', 7, 5000, 'require_reranker', NULL, false
         )",
        "54000",
        "semantic rerank candidates exceed the JSON allocation budget",
        "compact wide semantic rerank JSON node bomb",
    );
    shared_assert_sql_failure(
        "SELECT pgcontext.prepare_semantic_rerank(
             'semantic_raw_bounds', 'body', repeat('q', 65537), '[]'::jsonb,
             'linear-pair-v1', 7, 5000, 'require_reranker', NULL, false
         )",
        "54000",
        "semantic rerank query exceeds 65536 bytes",
        "oversized raw semantic rerank query",
    );
    shared_assert_sql_failure(
        "SELECT pgcontext.register_semantic_rerank_source(
             'semantic_raw_bounds', 'oversized', repeat('x', 64), 'source_version'
         )",
        "54000",
        "semantic rerank column name exceeds 63 bytes",
        "oversized raw semantic rerank column name",
    );

    let mut candidates = semantic_candidates("semantic_raw_bounds");
    candidates.0[1]["point_id"] = candidates.0[0]["point_id"].clone();
    shared_assert_sql_failure(
        &format!(
            "SELECT pgcontext.prepare_semantic_rerank(
                 'semantic_raw_bounds', 'body', 'postgres', '{}'::jsonb,
                 'linear-pair-v1', 7, 5000, 'require_reranker', NULL, false
             )",
            candidates.0.to_string().replace('\'', "''")
        ),
        "22023",
        "semantic rerank candidate identity or score is invalid",
        "duplicate semantic rerank point identity",
    );
}

#[pg_test]
fn semantic_rerank_rejects_a_query_valid_but_unencodable_worker_envelope() {
    Spi::run(
        "CREATE TABLE public.semantic_wire_escape (
             id bigint PRIMARY KEY,
             body text NOT NULL,
             source_version bigint NOT NULL DEFAULT 1
         );
         INSERT INTO public.semantic_wire_escape (id, body)
         SELECT id, repeat(chr(1), 32768)
           FROM generate_series(1, 110) AS id;
         SELECT pgcontext.create_collection(
             'semantic_wire_escape', 'public.semantic_wire_escape'
         );
         SELECT pgcontext.backfill_points('semantic_wire_escape', 200);
         SELECT pgcontext.register_semantic_rerank_source(
             'semantic_wire_escape', 'body', 'body', 'source_version'
         );",
    )
    .expect("escaped-wire fixture should be created");
    shared_assert_sql_failure(
        "SELECT pgcontext.prepare_semantic_rerank(
             'semantic_wire_escape', 'body', 'query',
             (
                 SELECT jsonb_agg(
                     jsonb_build_object(
                         'occurrence_id', points.source_key::bigint,
                         'point_id', points.point_id,
                         'fused_rank', points.source_key::bigint,
                         'fused_score', 0.1,
                         'contributions', jsonb_build_array(
                             jsonb_build_object(
                                 'profile', 'fixture', 'rank', 1,
                                 'native_score', 0.1, 'weight', 1.0,
                                 'contribution', 0.01
                             )
                         )
                     ) ORDER BY points.source_key::bigint
                 )
                   FROM pgcontext._visible_collection_points AS points
                  WHERE points.collection_id = (
                            SELECT collection_id
                              FROM pgcontext._visible_collections
                             WHERE collection_name = 'semantic_wire_escape'
                        )
             ),
             'linear-pair-v1', 7, 5000, 'require_reranker', NULL, false
         )",
        "54000",
        "semantic rerank envelope exceeds the encoded wire byte budget",
        "high-escape semantic rerank worker envelope",
    );
}
