#[pg_test]
fn register_model_version_records_catalog_row() {
    create_model_collection("m10_model_docs");

    let registered = model_version_rows(
        "SELECT collection_name,
                model_name,
                model_version,
                dimensions,
                metric,
                is_active
           FROM pgcontext.register_model_version(
             'm10_model_docs',
             'text-embedding-3-small',
             '2026-07-02',
             1536,
             'cosine'
           )",
    );

    assert_eq!(
        registered,
        vec![(
            "m10_model_docs".to_owned(),
            "text-embedding-3-small".to_owned(),
            "2026-07-02".to_owned(),
            1536,
            "cosine".to_owned(),
            true,
        )]
    );

    let listed = model_version_rows(
        "SELECT collection_name,
                model_name,
                model_version,
                dimensions,
                metric,
                is_active
           FROM pgcontext.model_versions()
          WHERE collection_name = 'm10_model_docs'",
    );

    assert_eq!(listed, registered);
}

#[pg_test]
#[should_panic(expected = "model version already registered: text-embedding@v1")]
fn register_model_version_rejects_duplicates() {
    create_model_collection("m10_model_duplicate");
    Spi::run(
        "SELECT pgcontext.register_model_version(
            'm10_model_duplicate',
            'text-embedding',
            'v1',
            3,
            'l2'
        )",
    )
    .expect("initial model version should be registered");
    Spi::run(
        "SELECT pgcontext.register_model_version(
            'm10_model_duplicate',
            'text-embedding',
            'v1',
            3,
            'l2'
        )",
    )
    .expect("duplicate model version should fail");
}

#[pg_test]
#[should_panic(expected = "collection does not exist: m10_model_missing")]
fn register_model_version_rejects_missing_collections() {
    Spi::run(
        "SELECT pgcontext.register_model_version(
            'm10_model_missing',
            'text-embedding',
            'v1',
            3,
            'l2'
        )",
    )
    .expect("missing collection should fail");
}

#[pg_test]
#[should_panic(expected = "invalid vector dimensions: 0")]
fn register_model_version_rejects_invalid_dimensions() {
    create_model_collection("m10_model_bad_dimensions");
    Spi::run(
        "SELECT pgcontext.register_model_version(
            'm10_model_bad_dimensions',
            'text-embedding',
            'v1',
            0,
            'l2'
        )",
    )
    .expect("invalid dimensions should fail");
}

#[pg_test]
#[should_panic(expected = "unsupported distance metric: hamming")]
fn register_model_version_rejects_unsupported_metrics() {
    create_model_collection("m10_model_bad_metric");
    Spi::run(
        "SELECT pgcontext.register_model_version(
            'm10_model_bad_metric',
            'text-embedding',
            'v1',
            3,
            'hamming'
        )",
    )
    .expect("unsupported metric should fail");
}

fn create_model_collection(collection_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL
         )"
    ))
    .expect("model source table should be created");
    Spi::run(&format!(
        "SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}')"
    ))
    .expect("model collection should be created");
}

type ModelVersionTestRow = (String, String, String, i32, String, bool);

fn model_version_rows(sql: &str) -> Vec<ModelVersionTestRow> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push((
                row.get::<String>(1)?
                    .expect("collection_name should not be null"),
                row.get::<String>(2)?.expect("model_name should not be null"),
                row.get::<String>(3)?
                    .expect("model_version should not be null"),
                row.get::<i32>(4)?.expect("dimensions should not be null"),
                row.get::<String>(5)?.expect("metric should not be null"),
                row.get::<bool>(6)?.expect("is_active should not be null"),
            ));
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("model version rows should be returned")
}
