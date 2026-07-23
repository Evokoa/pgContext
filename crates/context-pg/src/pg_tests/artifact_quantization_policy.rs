#[pg_test]
fn quantized_artifact_publication_rejects_a_stale_codebook_policy() {
    create_search_collection("stage_d_policy_binding");
    Spi::run(
        "SELECT pgcontext.configure_vector(
             'stage_d_policy_binding',
             'embedding',
             '{}'::jsonb,
             '{\"mode\":\"binary\"}'::jsonb,
             'ready'
         )",
    )
    .expect("binary policy should configure");
    upsert_search_points("stage_d_policy_binding", &["10", "20", "30"]);
    let job_id = start_artifact_build_job("stage_d_policy_binding", "mmap", "bound", 0);
    Spi::run(&format!("SELECT pgcontext.run_build_job({job_id}, 1)"))
        .expect("quantized build job should complete");
    Spi::run(&format!(
        "CREATE TEMP TABLE stage_d_stale_segment AS
         SELECT pgcontext.build_mmap_hnsw_artifact({job_id}) AS bytes"
    ))
    .expect("binary artifact should be retained for the policy-change check");
    Spi::run(
        "SELECT pgcontext.configure_vector(
             'stage_d_policy_binding',
             'embedding',
             '{}'::jsonb,
             '{\"mode\":\"scalar\",\"levels\":2}'::jsonb,
             'ready'
         )",
    )
    .expect("scalar policy should replace binary policy");

    shared_assert_sql_failure(
        &format!(
            "SELECT *
               FROM pgcontext.publish_artifact_segment_file(
                    {job_id},
                    (SELECT bytes FROM stage_d_stale_segment)
               )"
        ),
        "22023",
        "artifact quantization policy mismatch: persisted codebook/codes do not match registered policy: expected scalar, got binary",
        "quantization policy binding",
    );
}
