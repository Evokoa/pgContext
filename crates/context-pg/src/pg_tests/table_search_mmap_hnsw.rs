#[pg_test]
fn table_search_mmap_hnsw_artifact_rechecks_decoded_candidates() {
    create_search_collection("m13_mmap_hnsw_search");
    upsert_search_points("m13_mmap_hnsw_search", &["10", "20", "30"]);
    publish_mmap_hnsw_artifact(
        "m13_mmap_hnsw_search",
        "view-a",
        &[
            ("10", &[3.0, 0.0], &[1]),
            ("20", &[1.0, 0.0], &[0, 2]),
            ("30", &[2.0, 0.0], &[1]),
        ],
    );

    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_mmap_hnsw_artifact(
                'm13_mmap_hnsw_search',
                'view-a',
                '[0,0]'::vector,
                4096,
                3,
                2
           )",
    );

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, "20");
    assert_eq!(rows[0].2, 1.0);
    assert_eq!(rows[1].1, "30");
    assert_eq!(rows[1].2, 2.0);
}

#[pg_test]
fn mmap_hnsw_internal_candidate_helper_rejects_direct_sql_calls() {
    shared_assert_sql_failure(
        "SELECT *
           FROM pgcontext._mmap_hnsw_artifact_candidates(
                'direct-call-probe',
                'artifact',
                '[0,0]'::vector,
                4096,
                1,
                1
           )",
        "42501",
        "pgcontext internal mapped HNSW candidate helper cannot be called directly",
        "direct mapped HNSW candidate helper call",
    );
}

#[pg_test]
fn source_built_mmap_graph_is_navigable() {
    create_search_collection("m13_mmap_source_built_graph");
    upsert_search_points("m13_mmap_source_built_graph", &["10", "20", "30"]);
    let job_id = start_artifact_build_job(
        "m13_mmap_source_built_graph",
        "mmap",
        "source-built",
        0,
    );
    Spi::run(&format!("SELECT pgcontext.run_build_job({job_id}, 1)"))
        .expect("source-built mmap job should complete");
    let published = artifact_file_rows(&format!(
        "SELECT artifact_id,
                collection_name,
                build_job_id,
                artifact_kind,
                artifact_name,
                target_name,
                segment_kind,
                format_version,
                payload_bytes,
                checksum,
                relative_path,
                lifecycle_state
           FROM pgcontext.publish_artifact_segment_file(
                {job_id},
                pgcontext.build_mmap_hnsw_artifact({job_id})
           )"
    ));
    assert_eq!(published.len(), 1);

    let base_neighbor_count = Spi::get_one::<i64>(&format!(
        "SELECT base_neighbor_count
           FROM pgcontext.validate_hnsw_graph_artifact(
                pgcontext.build_mmap_hnsw_artifact({job_id})
           )"
    ))
    .expect("source-built graph metadata should load")
    .expect("source-built graph metadata should return one row");
    assert!(base_neighbor_count > 0);

    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_mmap_hnsw_artifact(
                'm13_mmap_source_built_graph',
                'source-built',
                '[0,0]'::pgcontext.vector,
                65536,
                4,
                2
           )",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, "20");
    assert_eq!(rows[1].1, "30");
}

#[pg_test]
fn quantized_source_built_graphs_use_v2_and_exact_source_rerank() {
    for (suffix, options) in [
        ("binary", r#"{"mode":"binary"}"#),
        ("scalar", r#"{"mode":"scalar","levels":2}"#),
        ("pq", r#"{"mode":"pq","subvector_dimensions":1}"#),
    ] {
        let collection = format!("stage_d_quantized_{suffix}");
        create_search_collection(&collection);
        Spi::run(&format!(
            "SELECT pgcontext.configure_vector(
                 '{collection}',
                 'embedding',
                 '{{}}'::jsonb,
                 '{options}'::jsonb,
                 'ready'
             )"
        ))
        .expect("quantized vector policy should configure");
        upsert_search_points(&collection, &["10", "20", "30"]);
        let job_id = start_artifact_build_job(&collection, "mmap", "quantized", 0);
        Spi::run(&format!("SELECT pgcontext.run_build_job({job_id}, 1)"))
            .expect("quantized mmap job should complete");

        let payload_version = Spi::get_one::<i32>(&format!(
            "SELECT pg_catalog.get_byte(
                 pgcontext.build_mmap_hnsw_artifact({job_id}),
                 48
             )"
        ))
        .expect("quantized graph payload version should load");
        assert_eq!(payload_version, Some(2));

        let published = artifact_file_rows(&format!(
            "SELECT artifact_id,
                    collection_name,
                    build_job_id,
                    artifact_kind,
                    artifact_name,
                    target_name,
                    segment_kind,
                    format_version,
                    payload_bytes,
                    checksum,
                    relative_path,
                    lifecycle_state
               FROM pgcontext.publish_artifact_segment_file(
                    {job_id},
                    pgcontext.build_mmap_hnsw_artifact({job_id})
               )"
        ));
        assert_eq!(published.len(), 1);

        let rows = table_search_rows(&format!(
            "SELECT point_id, source_key, score
               FROM pgcontext.search_mmap_hnsw_artifact(
                    '{collection}',
                    'quantized',
                    '[0,0]'::pgcontext.vector,
                    65536,
                    3,
                    2
               )"
        ));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].1, "20");
        assert_eq!(rows[0].2, 1.0);
        assert_eq!(rows[1].1, "30");
        assert_eq!(rows[1].2, 2.0);
        if suffix == "binary" {
            shared_assert_sql_failure(
                &format!(
                    "SELECT *
                       FROM pgcontext.search_mmap_hnsw_artifact(
                            '{collection}',
                            'quantized',
                            '[0,0]'::pgcontext.vector,
                            65536,
                            2,
                            2
                       )"
                ),
                "22023",
                "quantized mmap HNSW candidate_limit 2 must exceed final limit 2",
                "quantized candidate oversampling",
            );
        }
    }
}

#[pg_test]
fn table_search_mmap_hnsw_artifact_rechecks_updated_source_rows() {
    create_search_collection("m13_mmap_hnsw_source_recheck");
    upsert_search_points("m13_mmap_hnsw_source_recheck", &["10", "20", "30"]);
    publish_mmap_hnsw_artifact(
        "m13_mmap_hnsw_source_recheck",
        "view-a",
        &[
            ("10", &[3.0, 0.0], &[1]),
            ("20", &[1.0, 0.0], &[0, 2]),
            ("30", &[2.0, 0.0], &[1]),
        ],
    );
    Spi::run(
        "UPDATE public.m13_mmap_hnsw_source_recheck
            SET embedding = '[0,0]'::vector
          WHERE id = 30",
    )
    .expect("mmap source row should be updated after artifact publication");

    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_mmap_hnsw_artifact(
                'm13_mmap_hnsw_source_recheck',
                'view-a',
                '[0,0]'::vector,
                4096,
                3,
                2
           )",
    );

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, "30");
    assert_eq!(rows[0].2, 0.0);
    assert_eq!(rows[1].1, "20");
    assert_eq!(rows[1].2, 1.0);
}

#[pg_test]
fn table_search_mmap_hnsw_artifact_merges_points_added_after_the_generation() {
    create_search_collection("m13_mmap_hnsw_mutable_delta");
    upsert_search_points("m13_mmap_hnsw_mutable_delta", &["10", "20", "30"]);
    publish_mmap_hnsw_artifact(
        "m13_mmap_hnsw_mutable_delta",
        "view-a",
        &[
            ("10", &[3.0, 0.0], &[1]),
            ("20", &[1.0, 0.0], &[0, 2]),
            ("30", &[2.0, 0.0], &[1]),
        ],
    );
    Spi::run(
        "INSERT INTO public.m13_mmap_hnsw_mutable_delta (id, embedding)
         VALUES (40, '[0.25,0]'::vector)",
    )
    .expect("mutable delta source row should insert");
    upsert_search_points("m13_mmap_hnsw_mutable_delta", &["40"]);

    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_mmap_hnsw_artifact(
                'm13_mmap_hnsw_mutable_delta',
                'view-a',
                '[0,0]'::vector,
                4096,
                3,
                1
           )",
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, "40");
    assert_eq!(rows[0].2, 0.25);
}

#[pg_test]
fn table_search_mmap_hnsw_artifact_rechecks_full_candidate_cap_for_updated_source_rows() {
    create_search_collection("m13_mmap_hnsw_source_recheck_cap");
    upsert_search_points("m13_mmap_hnsw_source_recheck_cap", &["10", "20", "30"]);
    publish_mmap_hnsw_artifact(
        "m13_mmap_hnsw_source_recheck_cap",
        "view-a",
        &[
            ("10", &[1.0, 0.0], &[1]),
            ("20", &[2.0, 0.0], &[0, 2]),
            ("30", &[3.0, 0.0], &[1]),
        ],
    );
    Spi::run(
        "UPDATE public.m13_mmap_hnsw_source_recheck_cap
            SET embedding = '[0,0]'::vector
          WHERE id = 30",
    )
    .expect("mmap source row outside the first result prefix should be updated");

    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_mmap_hnsw_artifact(
                'm13_mmap_hnsw_source_recheck_cap',
                'view-a',
                '[0,0]'::vector,
                4096,
                3,
                2
           )",
    );

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, "30");
    assert_eq!(rows[0].2, 0.0);
    assert_eq!(rows[1].1, "20");
    assert_eq!(rows[1].2, 1.0);
}

#[pg_test]
fn table_search_mmap_hnsw_artifact_recheck_excludes_deleted_points() {
    create_search_collection("m13_mmap_hnsw_deleted");
    upsert_search_points("m13_mmap_hnsw_deleted", &["10", "20", "30"]);
    publish_mmap_hnsw_artifact(
        "m13_mmap_hnsw_deleted",
        "view-a",
        &[
            ("10", &[3.0, 0.0], &[1]),
            ("20", &[1.0, 0.0], &[0, 2]),
            ("30", &[2.0, 0.0], &[1]),
        ],
    );
    Spi::run("SELECT pgcontext.delete_points('m13_mmap_hnsw_deleted', ARRAY['20'])")
        .expect("mmap artifact candidate should be deleted");

    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_mmap_hnsw_artifact(
                'm13_mmap_hnsw_deleted',
                'view-a',
                '[0,0]'::vector,
                4096,
                3,
                3
           )",
    );

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, "30");
    assert_eq!(rows[0].2, 2.0);
    assert_eq!(rows[1].1, "10");
    assert_eq!(rows[1].2, 3.0);
}

#[pg_test]
fn table_search_mmap_hnsw_artifact_fills_from_candidate_cap_after_deleted_top_candidate() {
    create_search_collection("m13_mmap_hnsw_expand_deleted");
    upsert_search_points("m13_mmap_hnsw_expand_deleted", &["10", "20", "30"]);
    publish_mmap_hnsw_artifact(
        "m13_mmap_hnsw_expand_deleted",
        "view-a",
        &[
            ("10", &[0.0, 0.0], &[1]),
            ("20", &[1.0, 0.0], &[0, 2]),
            ("30", &[2.0, 0.0], &[1]),
        ],
    );
    Spi::run("SELECT pgcontext.delete_points('m13_mmap_hnsw_expand_deleted', ARRAY['10'])")
        .expect("top mmap artifact candidate should be deleted before search");

    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_mmap_hnsw_artifact(
                'm13_mmap_hnsw_expand_deleted',
                'view-a',
                '[0,0]'::vector,
                4096,
                3,
                2
           )",
    );

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, "20");
    assert_eq!(rows[0].2, 1.0);
    assert_eq!(rows[1].1, "30");
    assert_eq!(rows[1].2, 2.0);
}

#[pg_test]
fn table_search_mmap_hnsw_artifact_excludes_missing_source_rows() {
    create_search_collection("m13_mmap_hnsw_source_deleted");
    upsert_search_points("m13_mmap_hnsw_source_deleted", &["10", "20", "30"]);
    publish_mmap_hnsw_artifact(
        "m13_mmap_hnsw_source_deleted",
        "view-a",
        &[
            ("10", &[3.0, 0.0], &[1]),
            ("20", &[1.0, 0.0], &[0, 2]),
            ("30", &[2.0, 0.0], &[1]),
        ],
    );
    Spi::run("DELETE FROM public.m13_mmap_hnsw_source_deleted WHERE id = 20")
        .expect("mmap source row should be deleted after artifact publication");

    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_mmap_hnsw_artifact(
                'm13_mmap_hnsw_source_deleted',
                'view-a',
                '[0,0]'::vector,
                4096,
                3,
                3
           )",
    );

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, "30");
    assert_eq!(rows[0].2, 2.0);
    assert_eq!(rows[1].1, "10");
    assert_eq!(rows[1].2, 3.0);
}

#[pg_test]
fn table_search_mmap_hnsw_artifact_limits_requested_recheck_fanout() {
    create_search_collection("m13_mmap_hnsw_budget");
    upsert_search_points("m13_mmap_hnsw_budget", &["10", "20", "30"]);
    publish_mmap_hnsw_artifact(
        "m13_mmap_hnsw_budget",
        "view-a",
        &[
            ("10", &[3.0, 0.0], &[1]),
            ("20", &[1.0, 0.0], &[0, 2]),
            ("30", &[2.0, 0.0], &[1]),
        ],
    );
    Spi::run(
        "SELECT pgcontext.configure_collection_limits(
            'm13_mmap_hnsw_budget',
            true,
            NULL,
            NULL,
            NULL,
            NULL,
            NULL,
            2,
            NULL,
            NULL
        )",
    )
    .expect("strict candidate budget should be configured");

    let rows = table_search_rows(
        "SELECT point_id, source_key, score
           FROM pgcontext.search_mmap_hnsw_artifact(
                'm13_mmap_hnsw_budget',
                'view-a',
                '[0,0]'::vector,
                4096,
                2,
                2
           )",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, "20");
    assert_eq!(rows[1].1, "30");

    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.search_mmap_hnsw_artifact(
             'm13_mmap_hnsw_budget',
             'view-a',
             '[0,0]'::vector,
             4096,
             3,
             2
         )",
        "54000",
        "collection m13_mmap_hnsw_budget max_candidate_budget 2 exceeded: 3",
        "mmap artifact search requested candidate budget",
    );
}

#[pg_test]
fn table_search_mmap_hnsw_artifact_rejects_source_table_drift() {
    create_search_collection("m13_mmap_hnsw_source_drift");
    upsert_search_points("m13_mmap_hnsw_source_drift", &["10"]);
    publish_mmap_hnsw_artifact(
        "m13_mmap_hnsw_source_drift",
        "view-a",
        &[("10", &[3.0, 0.0], &[])],
    );
    Spi::run(
        "ALTER TABLE public.m13_mmap_hnsw_source_drift
            RENAME TO m13_mmap_hnsw_source_drift_old",
    )
    .expect("mmap source table should be renamed away");

    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.search_mmap_hnsw_artifact(
             'm13_mmap_hnsw_source_drift',
             'view-a',
             '[0,0]'::vector,
             4096,
             1,
             1
         )",
        "42P01",
        "registered source table drifted: public.m13_mmap_hnsw_source_drift",
        "mmap artifact source table drift",
    );
}

#[pg_test]
fn table_search_mmap_hnsw_artifact_rejects_not_ready_artifacts() {
    create_search_collection("m13_mmap_hnsw_not_ready");
    upsert_search_points("m13_mmap_hnsw_not_ready", &["10"]);
    let job_id = completed_artifact_build_job("m13_mmap_hnsw_not_ready", "mmap", "metadata-only");
    artifact_manifest_rows(&format!(
        "SELECT artifact_id,
                collection_name,
                build_job_id,
                artifact_kind,
                artifact_name,
                target_name,
                segment_kind,
                format_version,
                payload_bytes,
                checksum,
                lifecycle_state
           FROM pgcontext.publish_artifact_segment(
                {job_id},
                pgcontext.encode_artifact_segment(
                    'hnsw_graph',
                    decode('{}', 'hex')
                )
           )",
        hnsw_payload_hex("m13_mmap_hnsw_not_ready", &[("10", &[3.0, 0.0], &[])])
    ));

    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.search_mmap_hnsw_artifact(
             'm13_mmap_hnsw_not_ready',
             'metadata-only',
             '[0,0]'::vector,
             4096,
             1,
             1
        )",
        "55000",
        "serving-ready mmap artifact not found: m13_mmap_hnsw_not_ready/metadata-only",
        "mmap artifact search should reject metadata-only artifacts",
    );
}

#[pg_test]
fn table_search_mmap_hnsw_artifact_rejects_mapped_byte_budget() {
    create_search_collection("m13_mmap_hnsw_mapped_budget");
    upsert_search_points("m13_mmap_hnsw_mapped_budget", &["10"]);
    publish_mmap_hnsw_artifact(
        "m13_mmap_hnsw_mapped_budget",
        "view-a",
        &[("10", &[3.0, 0.0], &[])],
    );

    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.search_mmap_hnsw_artifact(
             'm13_mmap_hnsw_mapped_budget',
             'view-a',
             '[0,0]'::vector,
             1,
             1,
             1
         )",
        "55000",
        "mmap artifact is not serving-ready: memory_budget_exceeded (artifact mapped bytes exceed the serving memory budget)",
        "mmap artifact mapped-byte budget",
    );
}

#[pg_test]
fn table_search_mmap_hnsw_artifact_rejects_missing_artifact_name() {
    create_search_collection("m13_mmap_hnsw_missing_artifact");
    upsert_search_points("m13_mmap_hnsw_missing_artifact", &["10"]);

    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.search_mmap_hnsw_artifact(
             'm13_mmap_hnsw_missing_artifact',
             'missing-artifact',
             '[0,0]'::vector,
             4096,
             1,
             1
         )",
        "55000",
        "serving-ready mmap artifact not found: m13_mmap_hnsw_missing_artifact/missing-artifact",
        "mmap artifact search missing artifact name",
    );
}

#[pg_test]
fn table_search_mmap_hnsw_artifact_rejects_negative_mapped_byte_budget() {
    create_search_collection("m13_mmap_hnsw_negative_mapped_budget");
    upsert_search_points("m13_mmap_hnsw_negative_mapped_budget", &["10"]);
    publish_mmap_hnsw_artifact(
        "m13_mmap_hnsw_negative_mapped_budget",
        "view-a",
        &[("10", &[3.0, 0.0], &[])],
    );

    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.search_mmap_hnsw_artifact(
             'm13_mmap_hnsw_negative_mapped_budget',
             'view-a',
             '[0,0]'::vector,
             -1,
             1,
             1
         )",
        "22023",
        "max_mapped_bytes must be non-negative: -1",
        "mmap artifact search negative mapped-byte budget",
    );
}

#[pg_test]
fn table_search_mmap_hnsw_artifact_rejects_unservable_files() {
    create_search_collection("m13_mmap_hnsw_unservable");
    upsert_search_points("m13_mmap_hnsw_unservable", &["10"]);

    let missing_artifact = publish_mmap_hnsw_artifact(
        "m13_mmap_hnsw_unservable",
        "missing",
        &[("10", &[3.0, 0.0], &[])],
    );
    remove_artifact_file(
        missing_artifact
            .relative_path
            .as_ref()
            .expect("missing test artifact should record a path"),
    );

    let checksum_artifact = publish_mmap_hnsw_artifact(
        "m13_mmap_hnsw_unservable",
        "checksum",
        &[("10", &[3.0, 0.0], &[])],
    );
    flip_last_artifact_file_byte(
        checksum_artifact
            .relative_path
            .as_ref()
            .expect("checksum test artifact should record a path"),
    );

    let metadata_artifact = publish_mmap_hnsw_artifact(
        "m13_mmap_hnsw_unservable",
        "metadata",
        &[("10", &[3.0, 0.0], &[])],
    );
    Spi::run(&format!(
        "UPDATE pgcontext._artifact_segments
            SET payload_bytes = payload_bytes + 1
          WHERE artifact_id = {}",
        metadata_artifact.artifact_id
    ))
    .expect("test should simulate mmap artifact metadata drift");

    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.search_mmap_hnsw_artifact(
             'm13_mmap_hnsw_unservable',
             'missing',
             '[0,0]'::vector,
             4096,
             1,
             1
         )",
        "55000",
        "mmap artifact is not serving-ready: artifact_missing (artifact file is missing)",
        "mmap artifact missing file search",
    );
    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.search_mmap_hnsw_artifact(
             'm13_mmap_hnsw_unservable',
             'checksum',
             '[0,0]'::vector,
             4096,
             1,
             1
         )",
        "55000",
        "mmap artifact is not serving-ready: checksum_mismatch (segment checksum mismatch)",
        "mmap artifact checksum search",
    );
    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.search_mmap_hnsw_artifact(
             'm13_mmap_hnsw_unservable',
             'metadata',
             '[0,0]'::vector,
             4096,
             1,
             1
         )",
        "55000",
        "mmap artifact is not serving-ready: metadata_mismatch (artifact file metadata differs from catalog)",
        "mmap artifact metadata drift search",
    );
}

#[pg_test]
fn table_search_mmap_hnsw_artifact_rejects_corrupt_payload() {
    create_search_collection("m13_mmap_hnsw_corrupt");
    upsert_search_points("m13_mmap_hnsw_corrupt", &["10"]);
    let job_id = completed_artifact_build_job("m13_mmap_hnsw_corrupt", "mmap", "bad");
    artifact_file_rows(&format!(
        "SELECT artifact_id,
                collection_name,
                build_job_id,
                artifact_kind,
                artifact_name,
                target_name,
                segment_kind,
                format_version,
                payload_bytes,
                checksum,
                relative_path,
                lifecycle_state
           FROM pgcontext.publish_artifact_segment_file(
                {job_id},
                pgcontext.encode_artifact_segment(
                    'hnsw_graph',
                    decode('5847435458484e5301000000010000000100000000000000', 'hex')
                )
           )"
    ));

    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.search_mmap_hnsw_artifact(
             'm13_mmap_hnsw_corrupt',
             'bad',
             '[0,0]'::vector,
             4096,
             1,
             1
         )",
        "XX001",
        "invalid HNSW graph payload magic",
        "corrupt mmap HNSW artifact payload",
    );
}

#[pg_test]
fn table_search_mmap_hnsw_artifact_denies_non_owner_collections() {
    acl_create_role("m13_mmap_hnsw_owner");
    acl_create_role("m13_mmap_hnsw_denied");
    acl_grant_api_access("m13_mmap_hnsw_owner");
    acl_grant_api_access("m13_mmap_hnsw_denied");

    with_acl_session_user("m13_mmap_hnsw_owner", || {
        create_search_collection("m13_mmap_hnsw_acl");
        upsert_search_points("m13_mmap_hnsw_acl", &["10"]);
    });
    publish_mmap_hnsw_artifact(
        "m13_mmap_hnsw_acl",
        "view-a",
        &[("10", &[3.0, 0.0], &[])],
    );
    let owner_rows = with_acl_session_user("m13_mmap_hnsw_owner", || {
        table_search_rows(
            "SELECT point_id, source_key, score
               FROM pgcontext.search_mmap_hnsw_artifact(
                    'm13_mmap_hnsw_acl',
                    'view-a',
                    '[0,0]'::vector,
                    4096,
                    1,
                    1
               )",
        )
    });
    assert_eq!(owner_rows.len(), 1);
    assert_eq!(owner_rows[0].1, "10");

    Spi::run("GRANT SELECT ON public.m13_mmap_hnsw_acl TO m13_mmap_hnsw_denied")
        .expect("denied role should receive source-table select");
    with_acl_session_user("m13_mmap_hnsw_denied", || {
        acl_expect_insufficient_privilege(
            "SELECT * FROM pgcontext.search_mmap_hnsw_artifact(
                 'm13_mmap_hnsw_acl',
                 'view-a',
                 '[0,0]'::vector,
                 4096,
                 1,
                 1
             )",
            "permission denied for collection m13_mmap_hnsw_acl",
        );
    });
}

#[pg_test]
fn table_search_mmap_hnsw_artifact_rejects_source_table_select_denial() {
    create_search_collection("m13_mmap_hnsw_source_acl");
    upsert_search_points("m13_mmap_hnsw_source_acl", &["10"]);
    publish_mmap_hnsw_artifact(
        "m13_mmap_hnsw_source_acl",
        "view-a",
        &[("10", &[3.0, 0.0], &[])],
    );
    acl_create_role("m13_mmap_hnsw_source_acl_denied");
    acl_grant_api_access("m13_mmap_hnsw_source_acl_denied");
    Spi::run(
        "UPDATE pgcontext._collections
            SET owner_role = 'm13_mmap_hnsw_source_acl_denied'::regrole
          WHERE collection_name = 'm13_mmap_hnsw_source_acl';
         REVOKE ALL ON public.m13_mmap_hnsw_source_acl FROM PUBLIC;
         REVOKE ALL ON public.m13_mmap_hnsw_source_acl
           FROM m13_mmap_hnsw_source_acl_denied",
    )
    .expect("mmap source table privileges should be configured");

    with_acl_session_user("m13_mmap_hnsw_source_acl_denied", || {
        shared_assert_sql_failure(
            "SELECT * FROM pgcontext.search_mmap_hnsw_artifact(
                 'm13_mmap_hnsw_source_acl',
                 'view-a',
                 '[0,0]'::vector,
                 4096,
                 1,
                 1
             )",
            "42501",
            "permission denied for source table: public.m13_mmap_hnsw_source_acl",
            "mmap artifact source table privilege",
        );
    });
}

fn with_acl_session_user<T>(role_name: &str, action: impl FnOnce() -> T) -> T {
    acl_set_session_user(role_name);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(action));
    acl_reset_session_user();
    match result {
        Ok(value) => value,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}
