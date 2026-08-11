#[pg_test]
fn embedding_migration_tracks_profile_backfill_progress() {
    create_migration_fixture("m10_migration_docs");

    let created = migration_rows(
        "SELECT migration_id, collection_name, source_profile, target_profile,
                status::text, total_points, processed_points
           FROM pgcontext.create_embedding_migration(
             'm10_migration_docs', 'embed_v1', 'embed_v2', 42
           )",
    );
    assert_eq!(created.len(), 1);
    let migration_id = created[0].0;
    assert_eq!(created[0].1, "m10_migration_docs");
    assert_eq!(created[0].2, "embed_v1");
    assert_eq!(created[0].3, "embed_v2");
    assert_eq!(created[0].4, "Planned");
    assert_eq!(created[0].5, 42);
    assert_eq!(created[0].6, 0);

    let updated = migration_rows(&format!(
        "SELECT migration_id, collection_name, source_profile, target_profile,
                status::text, total_points, processed_points
           FROM pgcontext.update_embedding_migration({migration_id}, 42, 'completed')"
    ));
    assert_eq!(updated[0].4, "Completed");
    assert_eq!(updated[0].6, 42);

    let listed = migration_rows(
        "SELECT migration_id, collection_name, source_profile, target_profile,
                status::text, total_points, processed_points
           FROM pgcontext.embedding_migrations()
          WHERE collection_name = 'm10_migration_docs'",
    );
    assert_eq!(listed, updated);
}

#[pg_test]
#[should_panic(expected = "embedding profile does not exist: missing")]
fn embedding_migration_rejects_missing_profiles() {
    create_migration_fixture("m10_migration_missing_profile");
    Spi::run(
        "SELECT pgcontext.create_embedding_migration(
            'm10_migration_missing_profile', 'missing', 'embed_v2', 1
        )",
    )
    .expect("missing profile should fail");
}

#[pg_test]
#[should_panic(expected = "source and target embedding profiles must differ")]
fn embedding_migration_rejects_identical_profiles() {
    create_migration_fixture("m10_migration_same_profile");
    Spi::run(
        "SELECT pgcontext.create_embedding_migration(
            'm10_migration_same_profile', 'embed_v1', 'embed_v1', 1
        )",
    )
    .expect("identical profiles should fail");
}

#[pg_test]
#[should_panic(expected = "embedding migration progress exceeds total")]
fn embedding_migration_rejects_progress_past_total() {
    create_migration_fixture("m10_migration_progress");
    let migration_id = migration_rows(
        "SELECT migration_id, collection_name, source_profile, target_profile,
                status::text, total_points, processed_points
           FROM pgcontext.create_embedding_migration(
             'm10_migration_progress', 'embed_v1', 'embed_v2', 2
           )",
    )[0]
        .0;
    Spi::run(&format!(
        "SELECT pgcontext.update_embedding_migration({migration_id}, 3, 'running')"
    ))
    .expect("progress past total should fail");
}

fn create_migration_fixture(collection_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             embedding_v1 pgcontext.vector(3) NOT NULL,
             embedding_v2 pgcontext.vector(3) NOT NULL
         );
         CREATE INDEX {collection_name}_v1_hnsw ON public.{collection_name}
             USING pgcontext_hnsw (embedding_v1 pgcontext.vector_hnsw_ops);
         CREATE INDEX {collection_name}_v2_hnsw ON public.{collection_name}
             USING pgcontext_hnsw (embedding_v2 pgcontext.vector_hnsw_ops);
         SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}');"
    ))
    .expect("migration collection should be created");

    for (profile, column, revision, hash) in [
        ("embed_v1", "embedding_v1", "v1", "0123456789abcdef"),
        ("embed_v2", "embedding_v2", "v2", "fedcba9876543210"),
    ] {
        Spi::run(&format!(
            "SELECT pgcontext.register_embedding_profile(
                 '{collection_name}', '{profile}', '{column}',
                 'public.{collection_name}_{suffix}_hnsw',
                 jsonb_build_object(
                     'representation', 'dense', 'dimensions', 3,
                     'normalization', 'none', 'metric', 'l2',
                     'provider', 'fixture', 'model', 'embed-small',
                     'revision', '{revision}', 'input_template', '{{t}}',
                     'output_template', '{{v}}', 'bit_order', NULL,
                     'byte_order', NULL, 'scale', NULL, 'zero_point', NULL,
                     'configuration_hash', '{hash}'
                 )
             )",
            suffix = if column == "embedding_v1" { "v1" } else { "v2" }
        ))
        .expect("migration profile should be registered");
    }
}

type MigrationTestRow = (i64, String, String, String, String, i64, i64);

fn migration_rows(sql: &str) -> Vec<MigrationTestRow> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        rows.into_iter()
            .map(|row| {
                Ok((
                    row.get::<i64>(1)?.expect("migration_id"),
                    row.get::<String>(2)?.expect("collection_name"),
                    row.get::<String>(3)?.expect("source_profile"),
                    row.get::<String>(4)?.expect("target_profile"),
                    row.get::<String>(5)?.expect("status"),
                    row.get::<i64>(6)?.expect("total_points"),
                    row.get::<i64>(7)?.expect("processed_points"),
                ))
            })
            .collect::<Result<Vec<_>, spi::Error>>()
    })
    .expect("migration rows should be returned")
}
