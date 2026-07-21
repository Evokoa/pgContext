#[pg_test]
fn embedding_migration_tracks_backfill_progress() {
    create_migration_fixture("m10_migration_docs");

    let created = migration_rows(
        "SELECT migration_id,
                collection_name,
                source_model,
                source_version,
                target_model,
                target_version,
                status::text,
                total_points,
                processed_points
           FROM pgcontext.create_embedding_migration(
             'm10_migration_docs',
             'embed-small',
             'v1',
             'embed-small',
             'v2',
             42
           )",
    );

    assert_eq!(created.len(), 1);
    let migration_id = created[0].0;
    assert_eq!(created[0].1, "m10_migration_docs");
    assert_eq!(created[0].6, "Planned");
    assert_eq!(created[0].7, 42);
    assert_eq!(created[0].8, 0);

    let updated = migration_rows(&format!(
        "SELECT migration_id,
                collection_name,
                source_model,
                source_version,
                target_model,
                target_version,
                status::text,
                total_points,
                processed_points
           FROM pgcontext.update_embedding_migration({migration_id}, 42, 'completed')"
    ));

    assert_eq!(updated[0].0, migration_id);
    assert_eq!(updated[0].6, "Completed");
    assert_eq!(updated[0].8, 42);

    let listed = migration_rows(
        "SELECT migration_id,
                collection_name,
                source_model,
                source_version,
                target_model,
                target_version,
                status::text,
                total_points,
                processed_points
           FROM pgcontext.embedding_migrations()
          WHERE collection_name = 'm10_migration_docs'",
    );

    assert_eq!(listed, updated);
}

#[pg_test]
#[should_panic(expected = "model version does not exist: embed-small@missing")]
fn embedding_migration_rejects_missing_model_versions() {
    create_migration_fixture("m10_migration_missing_model");

    Spi::run(
        "SELECT pgcontext.create_embedding_migration(
            'm10_migration_missing_model',
            'embed-small',
            'missing',
            'embed-small',
            'v2',
            1
        )",
    )
    .expect("missing model version should fail");
}

#[pg_test]
#[should_panic(expected = "source and target model versions must differ")]
fn embedding_migration_rejects_identical_models() {
    create_migration_fixture("m10_migration_same_model");

    Spi::run(
        "SELECT pgcontext.create_embedding_migration(
            'm10_migration_same_model',
            'embed-small',
            'v1',
            'embed-small',
            'v1',
            1
        )",
    )
    .expect("identical model version should fail");
}

#[pg_test]
#[should_panic(expected = "embedding migration progress exceeds total")]
fn embedding_migration_rejects_progress_past_total() {
    create_migration_fixture("m10_migration_progress");
    let migration_id = migration_rows(
        "SELECT migration_id,
                collection_name,
                source_model,
                source_version,
                target_model,
                target_version,
                status::text,
                total_points,
                processed_points
           FROM pgcontext.create_embedding_migration(
             'm10_migration_progress',
             'embed-small',
             'v1',
             'embed-small',
             'v2',
             2
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
             embedding vector NOT NULL
         )"
    ))
    .expect("migration source table should be created");
    Spi::run(&format!(
        "SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}')"
    ))
    .expect("migration collection should be created");
    for version in ["v1", "v2"] {
        Spi::run(&format!(
            "SELECT pgcontext.register_model_version(
                '{collection_name}',
                'embed-small',
                '{version}',
                3,
                'l2'
            )"
        ))
        .expect("migration model version should be registered");
    }
}

type MigrationTestRow = (i64, String, String, String, String, String, String, i64, i64);

fn migration_rows(sql: &str) -> Vec<MigrationTestRow> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push((
                row.get::<i64>(1)?.expect("migration_id should not be null"),
                row.get::<String>(2)?
                    .expect("collection_name should not be null"),
                row.get::<String>(3)?
                    .expect("source_model should not be null"),
                row.get::<String>(4)?
                    .expect("source_version should not be null"),
                row.get::<String>(5)?
                    .expect("target_model should not be null"),
                row.get::<String>(6)?
                    .expect("target_version should not be null"),
                row.get::<String>(7)?.expect("status should not be null"),
                row.get::<i64>(8)?.expect("total_points should not be null"),
                row.get::<i64>(9)?
                    .expect("processed_points should not be null"),
            ));
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("migration rows should be returned")
}
