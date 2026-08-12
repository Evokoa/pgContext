#[pg_test]
fn semantic_rerank_rejects_untrusted_response_identity_membership_and_completeness() {
    semantic_rerank_fixture("semantic_invalid_response");
    let envelope = prepare_semantic_fixture(
        "semantic_invalid_response",
        "require_reranker",
        false,
    );
    let request_id = envelope.0["request_id"].as_i64().expect("request id");
    let call = |response: serde_json::Value| {
        format!(
            "SELECT pgcontext.finalize_semantic_rerank(
                 {request_id}, '{}'::jsonb, NULL
             )",
            response.to_string().replace('\'', "''")
        )
    };
    shared_assert_sql_failure(
        &call(json!({
            "version": 3,
            "request_id": request_id,
            "model": "wrong-model",
            "model_revision": 7,
            "scores": [
                {"occurrence_id": 11, "score": 0.8},
                {"occurrence_id": 12, "score": 0.7}
            ]
        })),
        "22023",
        "semantic rerank response failed validation: model_mismatch",
        "semantic rerank model substitution",
    );
    shared_assert_sql_failure(
        &call(json!({
            "version": 3,
            "request_id": request_id,
            "model": "linear-pair-v1",
            "model_revision": 7,
            "scores": [
                {"occurrence_id": 11, "score": 0.8},
                {"occurrence_id": 11, "score": 0.7}
            ]
        })),
        "22023",
        "semantic rerank response failed validation: duplicate_occurrence",
        "semantic rerank duplicate occurrence",
    );
    shared_assert_sql_failure(
        &call(json!({
            "version": 3,
            "request_id": request_id,
            "model": "linear-pair-v1",
            "model_revision": 7,
            "scores": [{"occurrence_id": 99, "score": 0.8}]
        })),
        "22023",
        "semantic rerank response failed validation: unknown_occurrence",
        "semantic rerank arbitrary occurrence injection",
    );
    shared_assert_sql_failure(
        &call(json!({
            "version": 3,
            "request_id": request_id,
            "model": "linear-pair-v1",
            "model_revision": 7,
            "scores": [{"occurrence_id": 11, "score": 0.8}]
        })),
        "22023",
        "semantic rerank response failed validation: incomplete",
        "semantic rerank incomplete required response",
    );
}

#[pg_test]
fn semantic_rerank_partial_policy_is_visible_and_rechecks_the_stored_filter() {
    semantic_rerank_fixture("semantic_partial");
    let candidates = semantic_candidates("semantic_partial");
    let envelope = Spi::get_one_with_args::<JsonB>(
        "SELECT pgcontext.prepare_semantic_rerank(
             'semantic_partial', 'body', 'postgres', $1,
             'linear-pair-v1', 7, 5000, 'require_reranker',
             '{\"must\":[{\"key\":\"tenant\",\"match\":\"red\"}]}'::jsonb,
             true
         )",
        &[candidates.into()],
    )
    .expect("filtered partial request should prepare")
    .expect("filtered partial envelope should exist");
    let request_id = envelope.0["request_id"].as_i64().expect("request id");
    let partial = Spi::get_one_with_args::<JsonB>(
        "SELECT pgcontext.finalize_semantic_rerank(
             $1,
             jsonb_build_object(
                 'version', 3,
                 'request_id', $1,
                 'model', 'linear-pair-v1',
                 'model_revision', 7,
                 'scores', jsonb_build_array(
                     jsonb_build_object('occurrence_id', 11, 'score', 0.8)
                 )
             ),
             NULL
         )",
        &[request_id.into()],
    )
    .expect("explicit partial response should succeed")
    .expect("partial result should exist");
    assert_eq!(partial.0["status"], "partial_reranked");
    assert_eq!(partial.0["completion"], "partial");
    assert_eq!(partial.0["results"].as_array().map(Vec::len), Some(1));

    let envelope = Spi::get_one_with_args::<JsonB>(
        "SELECT pgcontext.prepare_semantic_rerank(
             'semantic_partial', 'body', 'postgres', $1,
             'linear-pair-v1', 7, 5000, 'require_reranker',
             '{\"must\":[{\"key\":\"tenant\",\"match\":\"red\"}]}'::jsonb,
             false
         )",
        &[semantic_candidates("semantic_partial").into()],
    )
    .expect("second filtered request should prepare")
    .expect("second filtered envelope should exist");
    let request_id = envelope.0["request_id"].as_i64().expect("request id");
    Spi::run("UPDATE public.semantic_partial SET tenant = 'blue' WHERE id = 1")
        .expect("filter-visible row should change");
    shared_assert_sql_failure(
        &format!(
            "SELECT pgcontext.finalize_semantic_rerank(
                 {request_id},
                 jsonb_build_object(
                     'version', 3, 'request_id', {request_id},
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
        "semantic rerank filter change",
    );
}

#[pg_test]
fn semantic_rerank_request_storage_is_text_minimal_and_cleanup_is_bounded() {
    semantic_rerank_fixture("semantic_storage");
    let envelope = prepare_semantic_fixture(
        "semantic_storage",
        "allow_fused_fallback",
        false,
    );
    let request_id = envelope.0["request_id"].as_i64().expect("request id");
    shared_assert_sql_failure(
        &format!(
            "SELECT pgcontext._finalize_semantic_rerank_request(
                 {request_id}, decode('00', 'hex'), 'reranked', NULL, '{{}}'::jsonb
             )"
        ),
        "54000",
        "semantic rerank finalization exceeds its storage contract",
        "semantic rerank private finalization digest bound",
    );
    shared_assert_sql_failure(
        "SELECT pgcontext._insert_semantic_rerank_request(
             collections.collection_id,
             sources.rerank_source_id,
             sources.registration_revision,
             decode(pg_catalog.repeat('00', 32), 'hex'),
             'linear-pair-v1', 7, 'q', NULL, 'require_reranker', false,
             9223372036854775807,
             pg_catalog.jsonb_build_array(pg_catalog.jsonb_build_object(
                 'occurrence_id', 1,
                 'point_id', 1,
                 'source_version', 1,
                 'content_digest', pg_catalog.repeat('00', 32),
                 'fused_rank', 1,
                 'fused_score', 0.1,
                 'contributions', '[]'::jsonb,
                 'metadata', '[]'::jsonb
             ))
         )
           FROM pgcontext._visible_collections AS collections
           JOIN pgcontext._visible_semantic_rerank_sources AS sources
             USING (collection_id)
          WHERE collections.collection_name = 'semantic_storage'",
        "54000",
        "semantic rerank request exceeds its storage contract",
        "semantic rerank private request expiry bound",
    );
    shared_assert_sql_failure(
        "SELECT pgcontext._insert_semantic_rerank_request(
             collections.collection_id,
             sources.rerank_source_id,
             sources.registration_revision,
             decode(pg_catalog.repeat('00', 32), 'hex'),
             'linear-pair-v1', 7, 'q', NULL, 'require_reranker', false,
             9223372036854775807,
             pg_catalog.jsonb_build_array(
                 pg_catalog.jsonb_build_object('padding', pg_catalog.repeat('x', 4194305))
             )
         )
           FROM pgcontext._visible_collections AS collections
           JOIN pgcontext._visible_semantic_rerank_sources AS sources
             USING (collection_id)
          WHERE collections.collection_name = 'semantic_storage'",
        "54000",
        "semantic rerank request exceeds its storage contract",
        "semantic rerank private request size bound",
    );
    let text_columns = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM pg_catalog.pg_attribute
          WHERE attrelid = 'pgcontext._semantic_rerank_candidates'::regclass
            AND attnum > 0
            AND NOT attisdropped
            AND attname IN ('text', 'source_text', 'query_text', 'source_key')",
    )
    .expect("candidate catalog shape query should succeed")
    .expect("candidate catalog shape count should exist");
    assert_eq!(text_columns, 0);

    let fallback = Spi::get_one_with_args::<JsonB>(
        "SELECT pgcontext.finalize_semantic_rerank($1, NULL, 'unavailable')",
        &[request_id.into()],
    )
    .expect("visible fallback should succeed")
    .expect("fallback result should exist");
    assert_eq!(fallback.0["degraded_reason"], "unavailable");
    let removed = Spi::get_one::<i64>("SELECT pgcontext.cleanup_semantic_rerank_requests(1)")
        .expect("cleanup should succeed")
        .expect("cleanup count should exist");
    assert_eq!(removed, 1);
}
