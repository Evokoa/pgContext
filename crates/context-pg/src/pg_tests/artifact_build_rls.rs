#[pg_test]
fn mmap_artifact_build_applies_invoker_source_rls() {
    acl_create_role("stage_d_rls_table_owner");
    acl_create_role("stage_d_rls_collection_owner");
    acl_grant_api_access("stage_d_rls_table_owner");
    acl_grant_api_access("stage_d_rls_collection_owner");

    acl_set_session_user("stage_d_rls_table_owner");
    Spi::run(
        "CREATE TABLE public.stage_d_rls_build_docs (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL,
             tenant text NOT NULL
         )",
    )
    .expect("RLS artifact source should be created");
    Spi::run(
        "INSERT INTO public.stage_d_rls_build_docs (id, embedding, tenant)
         VALUES (1, '[1,0]'::vector, 'acme'),
                (2, '[2,0]'::vector, 'acme'),
                (3, '[3,0]'::vector, 'other')",
    )
    .expect("RLS artifact rows should be inserted");
    Spi::run("ALTER TABLE public.stage_d_rls_build_docs ENABLE ROW LEVEL SECURITY")
        .expect("RLS should be enabled");
    Spi::run("ALTER TABLE public.stage_d_rls_build_docs FORCE ROW LEVEL SECURITY")
        .expect("RLS should be forced");
    Spi::run(
        "CREATE POLICY stage_d_rls_build_tenant
            ON public.stage_d_rls_build_docs
         USING (tenant = current_setting('pgcontext_test.tenant', true))",
    )
    .expect("RLS artifact policy should be created");
    Spi::run(
        "GRANT SELECT ON public.stage_d_rls_build_docs TO stage_d_rls_collection_owner",
    )
    .expect("collection owner should receive source SELECT");
    acl_reset_session_user();

    acl_set_session_user("stage_d_rls_collection_owner");
    Spi::run(
        "SELECT pgcontext.create_collection(
             'stage_d_rls_build_docs',
             'public.stage_d_rls_build_docs'
         )",
    )
    .expect("RLS artifact collection should be created");
    Spi::run(
        "SELECT pgcontext.register_vector(
             'stage_d_rls_build_docs', 'embedding', 'embedding', 2, 'l2'
         )",
    )
    .expect("RLS artifact vector should be registered");
    Spi::run(
        "SELECT pgcontext.upsert_points(
             'stage_d_rls_build_docs', ARRAY['1', '2', '3']
         )",
    )
    .expect("RLS artifact points should be registered");
    let job_id = start_artifact_build_job("stage_d_rls_build_docs", "mmap", "rls", 0);
    Spi::run(&format!("SELECT pgcontext.run_build_job({job_id}, 1)"))
        .expect("RLS artifact build job should complete");
    let record_count = Spi::get_one::<i64>(&format!(
        "SET pgcontext_test.tenant = 'acme';
         SELECT record_count
           FROM pgcontext.validate_hnsw_graph_artifact(
                pgcontext.build_mmap_hnsw_artifact({job_id})
           )"
    ))
    .expect("RLS artifact should validate");
    assert_eq!(record_count, Some(2));
    acl_reset_session_user();
}
