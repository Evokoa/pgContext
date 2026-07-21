#[pg_test]
fn grouped_search_returns_nearest_rows_per_registered_column_group() {
    create_grouped_search_collection("m13_grouped_docs");
    upsert_grouped_search_points("m13_grouped_docs", &["10", "20", "30", "40"]);
    Spi::run(
        "SELECT pgcontext.register_filter_column('m13_grouped_docs', 'tenant_id', 'tenant_id')",
    )
    .expect("tenant filter should be registered");

    let rows = grouped_search_rows(
        "SELECT group_value, point_id, source_key, score
           FROM pgcontext.grouped_search(
                'm13_grouped_docs',
                '[0,0]'::vector,
                'tenant_id',
                1,
                10
           )",
    );

    assert_grouped_search_projection(
        &rows,
        &[
            ("acme", "10", 1.0),
            ("other", "30", 3.0),
        ],
    );
}

#[pg_test]
fn grouped_search_groups_jsonb_path_values_and_skips_missing_values() {
    create_grouped_search_collection("m13_grouped_jsonb");
    upsert_grouped_search_points("m13_grouped_jsonb", &["10", "20", "30", "40", "50"]);
    Spi::run(
        "SELECT pgcontext.register_jsonb_path(
            'm13_grouped_jsonb',
            'topic',
            'metadata',
            ARRAY['topic']
        )",
    )
    .expect("JSONB path should be registered");

    let rows = grouped_search_rows(
        "SELECT group_value, point_id, source_key, score
           FROM pgcontext.grouped_search(
                'm13_grouped_jsonb',
                '[0,0]'::vector,
                'topic',
                1,
                10
           )",
    );

    assert_grouped_search_projection(
        &rows,
        &[
            ("rust", "10", 1.0),
            ("postgres", "20", 2.0),
        ],
    );
}

#[pg_test]
fn grouped_search_applies_per_group_limit_and_point_id_tie_break() {
    create_grouped_search_collection("m13_grouped_ties");
    upsert_grouped_search_points("m13_grouped_ties", &["10", "20", "30", "40"]);
    Spi::run(
        "UPDATE public.m13_grouped_ties
            SET embedding = '[1,0]'::vector
          WHERE id IN (10, 20)",
    )
    .expect("tie fixture should be updated");
    Spi::run(
        "SELECT pgcontext.register_filter_column('m13_grouped_ties', 'tenant_id', 'tenant_id')",
    )
    .expect("tenant filter should be registered");

    let rows = grouped_search_rows(
        "SELECT group_value, point_id, source_key, score
           FROM pgcontext.grouped_search(
                'm13_grouped_ties',
                '[0,0]'::vector,
                'tenant_id',
                2,
                3
           )",
    );

    assert!(rows[0].1 < rows[1].1, "ties should break by point id");
    assert_grouped_search_projection(
        &rows,
        &[
            ("acme", "10", 1.0),
            ("acme", "20", 1.0),
            ("other", "30", 3.0),
        ],
    );
}

#[pg_test]
fn grouped_search_selects_named_dense_vector() {
    Spi::run(
        "CREATE TABLE public.m13_grouped_named_dense (
             id bigint PRIMARY KEY,
             title_embedding vector NOT NULL,
             body_embedding vector NOT NULL,
             tenant_id text NOT NULL
         )",
    )
    .expect("named grouped source table should be created");
    Spi::run(
        "INSERT INTO public.m13_grouped_named_dense
            (id, title_embedding, body_embedding, tenant_id)
         VALUES (10, '[1,0]'::vector, '[4,0]'::vector, 'acme'),
                (20, '[4,0]'::vector, '[1,0]'::vector, 'acme'),
                (30, '[2,0]'::vector, '[3,0]'::vector, 'other'),
                (40, '[3,0]'::vector, '[2,0]'::vector, 'other')",
    )
    .expect("named grouped rows should be inserted");
    Spi::run(
        "SELECT pgcontext.create_collection(
            'm13_grouped_named_dense',
            'public.m13_grouped_named_dense'
        )",
    )
    .expect("named grouped collection should be created");
    Spi::run(
        "SELECT pgcontext.register_vector(
            'm13_grouped_named_dense',
            'title',
            'title_embedding',
            2,
            'l2'
        )",
    )
    .expect("title vector should be registered");
    Spi::run(
        "SELECT pgcontext.register_vector(
            'm13_grouped_named_dense',
            'body',
            'body_embedding',
            2,
            'l2'
        )",
    )
    .expect("body vector should be registered");
    Spi::run(
        "SELECT pgcontext.register_filter_column(
            'm13_grouped_named_dense',
            'tenant_id',
            'tenant_id'
        )",
    )
    .expect("tenant group field should be registered");
    upsert_grouped_search_points("m13_grouped_named_dense", &["10", "20", "30", "40"]);

    let rows = grouped_search_rows(
        "SELECT group_value, point_id, source_key, score
           FROM pgcontext.grouped_search(
                'm13_grouped_named_dense',
                'body',
                '[0,0]'::vector,
                'tenant_id',
                1,
                10
           )",
    );

    assert_grouped_search_projection(&rows, &[("acme", "20", 1.0), ("other", "40", 2.0)]);
}

#[pg_test]
#[should_panic(expected = "unknown filter field: missing")]
fn grouped_search_rejects_unregistered_group_fields() {
    create_grouped_search_collection("m13_grouped_unknown");
    upsert_grouped_search_points("m13_grouped_unknown", &["10"]);
    Spi::run(
        "SELECT pgcontext.register_filter_column('m13_grouped_unknown', 'tenant_id', 'tenant_id')",
    )
    .expect("tenant filter should be registered");

    Spi::run(
        "SELECT pgcontext.grouped_search(
            'm13_grouped_unknown',
            '[0,0]'::vector,
            'missing',
            1,
            10
        )",
    )
    .expect("unregistered group field should be rejected");
}

#[pg_test]
#[should_panic(expected = "invalid search limit: 0")]
fn grouped_search_rejects_zero_group_limit() {
    create_grouped_search_collection("m13_grouped_bad_limit");
    upsert_grouped_search_points("m13_grouped_bad_limit", &["10"]);
    Spi::run(
        "SELECT pgcontext.register_filter_column(
            'm13_grouped_bad_limit',
            'tenant_id',
            'tenant_id'
        )",
    )
    .expect("tenant filter should be registered");

    Spi::run(
        "SELECT pgcontext.grouped_search(
            'm13_grouped_bad_limit',
            '[0,0]'::vector,
            'tenant_id',
            0,
            10
        )",
    )
    .expect("zero group limit should be rejected");
}

fn create_grouped_search_collection(collection_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL,
             tenant_id text NOT NULL,
             metadata jsonb NOT NULL
         )"
    ))
    .expect("grouped search source table should be created");
    Spi::run(&format!(
        "INSERT INTO public.{collection_name} (id, embedding, tenant_id, metadata)
         VALUES (10, '[1,0]'::vector, 'acme', '{{\"topic\":\"rust\"}}'::jsonb),
                (20, '[2,0]'::vector, 'acme', '{{\"topic\":\"postgres\"}}'::jsonb),
                (30, '[3,0]'::vector, 'other', '{{\"topic\":\"rust\"}}'::jsonb),
                (40, '[4,0]'::vector, 'other', '{{\"topic\":\"postgres\"}}'::jsonb),
                (50, '[5,0]'::vector, 'ignored', '{{\"other\":\"missing\"}}'::jsonb)"
    ))
    .expect("grouped search source rows should be inserted");
    Spi::run(&format!(
        "SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}')"
    ))
    .expect("grouped search collection should be created");
    Spi::run(&format!(
        "SELECT pgcontext.register_vector('{collection_name}', 'embedding', 'embedding', 2, 'l2')"
    ))
    .expect("grouped search vector should be registered");
}

fn upsert_grouped_search_points(collection_name: &str, keys: &[&str]) {
    let quoted_keys = keys
        .iter()
        .map(|key| format!("'{key}'"))
        .collect::<Vec<_>>()
        .join(",");
    Spi::run(&format!(
        "SELECT pgcontext.upsert_points('{collection_name}', ARRAY[{quoted_keys}])"
    ))
    .expect("grouped search points should be upserted");
}

fn grouped_search_rows(sql: &str) -> Vec<(String, i64, String, f32)> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push((
                row.get::<String>(1)?
                    .expect("group value should not be null"),
                row.get::<i64>(2)?.expect("point id should not be null"),
                row.get::<String>(3)?
                    .expect("source key should not be null"),
                row.get::<f32>(4)?.expect("score should not be null"),
            ));
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("grouped search query failed")
}

fn assert_grouped_search_projection(
    actual: &[(String, i64, String, f32)],
    expected: &[(&str, &str, f32)],
) {
    let projected = actual
        .iter()
        .map(|(group_value, _point_id, source_key, score)| {
            (group_value.as_str(), source_key.as_str(), *score)
        })
        .collect::<Vec<_>>();
    assert_eq!(projected, expected);
}
