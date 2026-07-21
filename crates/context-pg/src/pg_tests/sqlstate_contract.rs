#[pg_test]
fn sqlstate_contract_covers_vector_and_search_bad_paths() {
    assert_sqlstate(
        "SELECT '[]'::vector",
        PgSqlErrorCode::ERRCODE_INVALID_TEXT_REPRESENTATION,
    );
    assert_sqlstate(
        "SELECT pgcontext.l2_distance('[1]'::vector, '[1,2]'::vector)",
        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
    );
    assert_sqlstate(
        "SELECT * FROM pgcontext.search(
             '[1]'::vector,
             ARRAY[1, 2]::bigint[],
             ARRAY['[1]'::vector],
             'l2',
             1
         )",
        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
    );
    assert_sqlstate(
        "SELECT * FROM pgcontext.search(
             '[1]'::vector,
             ARRAY[1]::bigint[],
             ARRAY['[1]'::vector],
             'bad_metric',
             1
         )",
        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
    );
    assert_sqlstate(
        "SELECT * FROM pgcontext.search(
             '[1]'::vector,
             ARRAY[-1]::bigint[],
             ARRAY['[1]'::vector],
             'l2',
             1
         )",
        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
    );
}

#[pg_test]
fn sqlstate_contract_covers_collection_and_registration_bad_paths() {
    assert_sqlstate(
        "SELECT * FROM pgcontext.create_collection('bad-name')",
        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
    );

    Spi::run("SELECT * FROM pgcontext.create_collection('m0_sqlstate_duplicate')")
        .expect("initial collection creation should succeed");
    assert_sqlstate(
        "SELECT * FROM pgcontext.create_collection('m0_sqlstate_duplicate')",
        PgSqlErrorCode::ERRCODE_DUPLICATE_OBJECT,
    );

    assert_sqlstate(
        "SELECT * FROM pgcontext.collection_info('m0_sqlstate_missing')",
        PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
    );
    assert_sqlstate(
        "SELECT * FROM pgcontext.create_collection(
             'm0_sqlstate_missing_table',
             'public.m0_sqlstate_missing_table'
         )",
        PgSqlErrorCode::ERRCODE_UNDEFINED_TABLE,
    );

    create_sqlstate_source_table("m0_sqlstate_source");
    Spi::run(
        "SELECT * FROM pgcontext.create_collection(
             'm0_sqlstate_source',
             'public.m0_sqlstate_source'
         )",
    )
    .expect("source-table collection should be created");

    assert_sqlstate(
        "SELECT * FROM pgcontext.register_vector(
             'm0_sqlstate_source',
             'embedding',
             'missing_embedding',
             2,
             'l2'
         )",
        PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
    );
    assert_sqlstate(
        "SELECT * FROM pgcontext.register_vector(
             'm0_sqlstate_source',
             'embedding',
             'body',
             2,
             'l2'
         )",
        PgSqlErrorCode::ERRCODE_DATATYPE_MISMATCH,
    );
    assert_sqlstate(
        "SELECT * FROM pgcontext.register_vector(
             'm0_sqlstate_source',
             'embedding',
             'embedding',
             2,
             'hamming'
        )",
        PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
    );
    assert_sqlstate(
        "SELECT * FROM pgcontext.register_filter_column(
             'm0_sqlstate_source',
             'missing',
             'missing_status'
         )",
        PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
    );
    assert_sqlstate(
        "SELECT * FROM pgcontext.register_jsonb_path(
             'm0_sqlstate_source',
             'body_path',
             'body',
             ARRAY['topic']
         )",
        PgSqlErrorCode::ERRCODE_DATATYPE_MISMATCH,
    );
    Spi::run(
        "SELECT * FROM pgcontext.register_filter_column(
             'm0_sqlstate_source',
             'status',
             'status'
         )",
    )
    .expect("status filter should be registered");
    assert_sqlstate(
        "SELECT * FROM pgcontext.register_filter_column(
             'm0_sqlstate_source',
             'status',
             'status'
         )",
        PgSqlErrorCode::ERRCODE_DUPLICATE_OBJECT,
    );
    assert_sqlstate(
        "SELECT * FROM pgcontext.upsert_points('m0_sqlstate_source', ARRAY[''])",
        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
    );
    assert_sqlstate(
        "SELECT * FROM pgcontext.delete_points('m0_sqlstate_source', ARRAY[''])",
        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
    );
    assert_sqlstate(
        "SELECT pgcontext.drop_collection('bad-name')",
        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
    );
}

#[pg_test]
fn sqlstate_contract_covers_filter_and_operation_bad_paths() {
    create_sqlstate_source_table("m0_sqlstate_filter");
    Spi::run(
        "SELECT * FROM pgcontext.create_collection(
             'm0_sqlstate_filter',
             'public.m0_sqlstate_filter'
         )",
    )
    .expect("filter collection should be created");
    Spi::run(
        "SELECT * FROM pgcontext.register_vector(
             'm0_sqlstate_filter',
             'embedding',
             'embedding',
             2,
             'l2'
         )",
    )
    .expect("vector should be registered");

    assert_sqlstate(
        "SELECT * FROM pgcontext.search(
             'm0_sqlstate_filter',
             '[0,0]'::vector,
             '{\"must\":[{\"key\":\"missing\",\"match\":\"x\"}]}',
             1
         )",
        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
    );
    assert_sqlstate(
        "SELECT pgcontext.count(
             'm0_sqlstate_filter',
             '{\"must\":[{\"key\":\"missing\",\"match\":\"x\"}]}'
         )",
        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
    );
    assert_sqlstate(
        "SELECT * FROM pgcontext.search(
             'm0_sqlstate_filter',
             '[0,0]'::vector,
             '{\"must\":[{\"key\":\"missing\",\"match\":\"x\"}]}',
             ARRAY[1]::bigint[],
             1
        )",
        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
    );
    assert_sqlstate(
        "SELECT * FROM pgcontext.scroll('m0_sqlstate_filter', 'not-a-cursor', 1)",
        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
    );
    assert_sqlstate(
        "SELECT * FROM pgcontext.facet('m0_sqlstate_filter', 'missing', NULL, 1)",
        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
    );
    assert_sqlstate(
        "SELECT * FROM pgcontext.query(
             'm0_sqlstate_filter',
             '[0,0]'::vector,
             'text',
             'missing_body',
             1
         )",
        PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
    );
    assert_sqlstate(
        "SELECT * FROM pgcontext.recall_check(ARRAY[1]::bigint[], ARRAY[1]::bigint[], 1.5)",
        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
    );
    assert_sqlstate(
        "SELECT * FROM pgcontext.index_status('public.m0_sqlstate_missing_idx')",
        PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
    );
}

#[pg_test]
fn sqlstate_contract_covers_model_migration_and_telemetry_bad_paths() {
    Spi::run("SELECT * FROM pgcontext.create_collection('m0_sqlstate_ops')")
        .expect("operations collection should be created");

    assert_sqlstate(
        "SELECT * FROM pgcontext.register_model_version(
             'm0_sqlstate_ops',
             'model',
             'v1',
             2,
             'hamming'
         )",
        PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
    );
    Spi::run(
        "SELECT * FROM pgcontext.register_model_version(
             'm0_sqlstate_ops',
             'model',
             'v1',
             2,
             'l2'
         )",
    )
    .expect("source model version should be registered");
    assert_sqlstate(
        "SELECT * FROM pgcontext.register_model_version(
             'm0_sqlstate_ops',
             'model',
             'v1',
             2,
             'l2'
         )",
        PgSqlErrorCode::ERRCODE_DUPLICATE_OBJECT,
    );
    assert_sqlstate(
        "SELECT * FROM pgcontext.create_embedding_migration(
             'm0_sqlstate_ops',
             'model',
             'v1',
             'model',
             'missing',
             1
         )",
        PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
    );
    assert_sqlstate(
        "SELECT * FROM pgcontext.create_embedding_migration(
             'm0_sqlstate_ops',
             'model',
             'v1',
             'model',
             'v1',
             1
         )",
        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
    );
    assert_sqlstate(
        "SELECT * FROM pgcontext.update_embedding_migration(-1, 0, 'planned')",
        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
    );
    assert_sqlstate(
        "SELECT pgcontext.record_query_stat(
             'm0_sqlstate_ops',
             'cohort',
             'unknown',
             0,
             NULL,
             0.0
         )",
        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
    );
    assert_sqlstate(
        "SELECT pgcontext.record_query_stat(
             'm0_sqlstate_missing',
             'cohort',
             'search',
             0,
             NULL,
             0.0
         )",
        PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
    );
}

/// Renders a `PgSqlErrorCode` as its five-character SQLSTATE text, inverting
/// PostgreSQL's `MAKE_SQLSTATE` six-bit packing.
fn sqlstate_text(code: PgSqlErrorCode) -> String {
    let mut value = code as i32;
    (0..5)
        .map(|_| {
            let ch = char::from(u8::try_from(value & 0x3F).expect("six-bit char") + b'0');
            value >>= 6;
            ch
        })
        .collect()
}

fn assert_sqlstate(sql: &str, expected: PgSqlErrorCode) {
    // The probe must run inside a plpgsql exception block, not a Rust-side
    // PgTryBuilder catch: PgTryBuilder does not open a subtransaction, so
    // catching an error raised from inside a function that carries a
    // `SET search_path` proconfig skipped that function's GUC restore and
    // left its restricted search_path applied to the rest of the session —
    // every later unqualified `vector` reference in the test then failed
    // with "type vector does not exist". The plpgsql EXCEPTION clause rolls
    // back to an implicit savepoint, restoring GUC state.
    let expected_code = sqlstate_text(expected);
    Spi::run(&format!(
        r#"
        DO $$
        DECLARE
            actual_sqlstate text;
        BEGIN
            BEGIN
                PERFORM * FROM ({sql}) AS sqlstate_probe;
                RAISE EXCEPTION 'expected SQLSTATE {expected_code}, statement succeeded';
            EXCEPTION WHEN OTHERS THEN
                GET STACKED DIAGNOSTICS actual_sqlstate = RETURNED_SQLSTATE;
                IF actual_sqlstate <> '{expected_code}' THEN
                    RAISE EXCEPTION 'expected SQLSTATE {expected_code}, got %: %',
                        actual_sqlstate,
                        SQLERRM;
                END IF;
            END;
        END $$;
        "#
    ))
    .unwrap_or_else(|error| panic!("SQLSTATE probe should run for {sql}: {error}"));
}

fn create_sqlstate_source_table(table_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{table_name} (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL,
             status text,
             body text,
             metadata jsonb
         )"
    ))
    .expect("source table should be created");
}
