#[pg_test]
fn create_collection_records_catalog_metadata() {
    let rows = collection_rows(
        "SELECT collection_id, collection_name, owner_name, table_schema, table_name
           FROM pgcontext.create_collection('m2_catalog_create')",
    );

    assert_eq!(rows.len(), 1);
    assert!(rows[0].0 > 0);
    assert_eq!(rows[0].1, "m2_catalog_create");
    assert_eq!(rows[0].2, current_user());

    let info_rows = collection_rows(
        "SELECT collection_id, collection_name, owner_name, table_schema, table_name
           FROM pgcontext.collection_info('m2_catalog_create')",
    );
    assert_eq!(info_rows, rows);
}

#[pg_test]
fn drop_collection_removes_catalog_metadata() {
    Spi::run("SELECT pgcontext.create_collection('m2_catalog_drop')")
        .expect("create collection should succeed");

    let dropped = Spi::get_one::<bool>("SELECT pgcontext.drop_collection('m2_catalog_drop')")
        .expect("drop collection query failed")
        .expect("drop collection should return a boolean");
    assert!(dropped);

    let dropped_again = Spi::get_one::<bool>("SELECT pgcontext.drop_collection('m2_catalog_drop')")
        .expect("second drop collection query failed")
        .expect("drop collection should return a boolean");
    assert!(!dropped_again);
}

#[pg_test]
#[should_panic(expected = "collection already exists: m2_catalog_duplicate")]
fn create_collection_rejects_duplicates() {
    Spi::run("SELECT pgcontext.create_collection('m2_catalog_duplicate')")
        .expect("initial create collection should succeed");
    Spi::run("SELECT pgcontext.create_collection('m2_catalog_duplicate')")
        .expect("duplicate create collection should fail");
}

#[pg_test]
#[should_panic(
    expected = "invalid collection name: must contain only ASCII letters, digits, and underscores: \"bad-name\""
)]
fn create_collection_rejects_invalid_collection_names() {
    Spi::run("SELECT pgcontext.create_collection('bad-name')")
        .expect("invalid collection name should fail");
}

#[pg_test]
fn invalid_collection_names_use_invalid_filter_sqlstate() {
    let caught = PgTryBuilder::new(|| {
        Spi::run("SELECT pgcontext.create_collection('bad-name')")
            .expect("invalid collection name should fail");
        false
    })
    .catch_when(PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE, |_| true)
    .execute();

    assert!(caught);
}

#[pg_test]
#[should_panic(expected = "collection does not exist: m2_catalog_missing")]
fn collection_info_rejects_missing_collections() {
    Spi::run("SELECT pgcontext.collection_info('m2_catalog_missing')")
        .expect("missing collection info should fail");
}

fn collection_rows(sql: &str) -> Vec<(i64, String, String, Option<String>, Option<String>)> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push((
                row.get::<i64>(1)?.expect("collection_id should not be null"),
                row.get::<String>(2)?
                    .expect("collection_name should not be null"),
                row.get::<String>(3)?.expect("owner_name should not be null"),
                row.get::<String>(4)?,
                row.get::<String>(5)?,
            ));
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("collection rows query failed")
}

fn current_user() -> String {
    Spi::get_one::<String>("SELECT CURRENT_USER::text")
        .expect("current user query failed")
        .expect("current user should not be null")
}
