#[pg_test]
fn index_advisor_recommends_btree_for_unindexed_filter_column() {
    create_optimization_collection("m10_adv_btree");
    Spi::run("SELECT pgcontext.register_filter_column('m10_adv_btree', 'tenant_id', 'tenant_id')")
        .expect("advisor btree filter should be registered");

    let rows = index_advisor_rows(
        "SELECT collection_name,
                filter_key,
                column_name,
                recommendation::text,
                detail,
                suggested_sql
           FROM pgcontext.index_advisor('m10_adv_btree')",
    );

    let row = advisor_row_with_recommendation(&rows, "CreateBtreeIndex");
    assert_eq!(row.0, "m10_adv_btree");
    assert_eq!(row.1, Some("tenant_id".to_owned()));
    assert_eq!(row.2, Some("tenant_id".to_owned()));
    assert_eq!(row.4, "registered filter lacks a btree index");
    assert_eq!(
        row.5.as_deref(),
        Some("CREATE INDEX m10_adv_btree_tenant_id_btree_idx ON public.m10_adv_btree USING btree (tenant_id)")
    );
}

#[pg_test]
fn index_advisor_reports_no_action_for_existing_filter_index() {
    create_optimization_collection("m10_adv_indexed");
    Spi::run("SELECT pgcontext.register_filter_column('m10_adv_indexed', 'tenant_id', 'tenant_id')")
        .expect("advisor indexed filter should be registered");
    Spi::run("CREATE INDEX m10_adv_indexed_tenant_id_idx ON m10_adv_indexed (tenant_id)")
        .expect("advisor btree fixture index should be created");

    let rows = index_advisor_rows(
        "SELECT collection_name,
                filter_key,
                column_name,
                recommendation::text,
                detail,
                suggested_sql
           FROM pgcontext.index_advisor('m10_adv_indexed')",
    );

    let row = advisor_row_for_filter(&rows, "tenant_id");
    assert_eq!(row.3, "NoAction");
    assert_eq!(row.4, "btree index already covers registered filter");
    assert_eq!(row.5, None);
}

#[pg_test]
fn index_advisor_recommends_gin_for_unindexed_jsonb_filter() {
    Spi::run(
        "CREATE TABLE public.m10_adv_jsonb (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL,
             metadata jsonb NOT NULL
         )",
    )
    .expect("advisor jsonb fixture table should be created");
    Spi::run(
        "INSERT INTO public.m10_adv_jsonb (id, embedding, metadata)
         VALUES (10, '[1,2]'::vector, '{\"tenant\":\"acme\"}'::jsonb)",
    )
    .expect("advisor jsonb fixture row should be inserted");
    Spi::run("SELECT pgcontext.create_collection('m10_adv_jsonb', 'public.m10_adv_jsonb')")
        .expect("advisor jsonb collection should be created");
    Spi::run("SELECT pgcontext.register_vector('m10_adv_jsonb', 'embedding', 'embedding', 2, 'l2')")
        .expect("advisor jsonb vector should be registered");
    Spi::run(
        "SELECT pgcontext.register_jsonb_path(
            'm10_adv_jsonb',
            'tenant',
            'metadata',
            ARRAY['tenant']::text[]
         )",
    )
    .expect("advisor jsonb filter should be registered");

    let rows = index_advisor_rows(
        "SELECT collection_name,
                filter_key,
                column_name,
                recommendation::text,
                detail,
                suggested_sql
           FROM pgcontext.index_advisor('m10_adv_jsonb')",
    );

    let row = advisor_row_with_recommendation(&rows, "CreateGinIndex");
    assert_eq!(row.1, Some("tenant".to_owned()));
    assert_eq!(row.2, Some("metadata".to_owned()));
    assert_eq!(row.4, "registered filter lacks a gin index");
    assert_eq!(
        row.5.as_deref(),
        Some("CREATE INDEX m10_adv_jsonb_metadata_gin_idx ON public.m10_adv_jsonb USING gin (metadata)")
    );
}

#[pg_test]
fn index_advisor_handles_collection_without_source_table() {
    Spi::run("SELECT pgcontext.create_collection('m10_adv_no_source')")
        .expect("advisor no-source collection should be created");

    let rows = index_advisor_rows(
        "SELECT collection_name,
                filter_key,
                column_name,
                recommendation::text,
                detail,
                suggested_sql
           FROM pgcontext.index_advisor('m10_adv_no_source')",
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "m10_adv_no_source");
    assert_eq!(rows[0].3, "NoAction");
    assert_eq!(
        rows[0].4,
        "collection has no source table; no filter indexes can be advised"
    );
}

#[pg_test]
fn index_advisor_rejects_missing_collection_with_stable_sqlstate() {
    let caught = PgTryBuilder::new(|| {
        Spi::run("SELECT * FROM pgcontext.index_advisor('m10_adv_missing')")
            .expect("missing index advisor collection should fail");
        false
    })
    .catch_when(PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT, |_| true)
    .execute();

    assert!(caught, "missing index advisor collection must use SQLSTATE 42704");
}

type IndexAdvisorTestRow = (
    String,
    Option<String>,
    Option<String>,
    String,
    String,
    Option<String>,
);

fn index_advisor_rows(sql: &str) -> Vec<IndexAdvisorTestRow> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push((
                row.get::<String>(1)?
                    .expect("collection_name should not be null"),
                row.get::<String>(2)?,
                row.get::<String>(3)?,
                row.get::<String>(4)?
                    .expect("recommendation should not be null"),
                row.get::<String>(5)?.expect("detail should not be null"),
                row.get::<String>(6)?,
            ));
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("index advisor rows should be returned")
}

fn advisor_row_with_recommendation<'a>(
    rows: &'a [IndexAdvisorTestRow],
    recommendation: &str,
) -> &'a IndexAdvisorTestRow {
    rows.iter()
        .find(|row| row.3 == recommendation)
        .unwrap_or_else(|| panic!("advisor recommendation not found: {recommendation}"))
}

fn advisor_row_for_filter<'a>(
    rows: &'a [IndexAdvisorTestRow],
    filter_key: &str,
) -> &'a IndexAdvisorTestRow {
    rows.iter()
        .find(|row| row.1.as_deref() == Some(filter_key))
        .unwrap_or_else(|| panic!("advisor filter row not found: {filter_key}"))
}
