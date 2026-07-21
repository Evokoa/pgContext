#[pg_test]
fn pgvector_dense_typmods_accept_matching_dimensions_and_report_metadata() {
    Spi::run(
        "CREATE TEMP TABLE pgcontext_dense_typmod_items (
            embedding vector(3) NOT NULL
        )",
    )
    .expect("dense typmod table creation failed");
    Spi::run(
        "INSERT INTO pgcontext_dense_typmod_items (embedding)
         VALUES ('[1,2,3]'::vector)",
    )
    .expect("dense typmod row insert failed");

    let formatted_type = Spi::get_one::<String>(
        "SELECT format_type(atttypid, atttypmod)
           FROM pg_catalog.pg_attribute
          WHERE attrelid = 'pg_temp.pgcontext_dense_typmod_items'::regclass
            AND attname = 'embedding'",
    )
    .expect("dense typmod metadata query failed")
    .expect("dense typmod metadata should exist");

    assert_eq!(formatted_type, "vector(3)");
}

#[pg_test]
fn pgvector_dense_typmods_reject_dimension_mismatches_with_sqlstate() {
    Spi::run(
        "CREATE TEMP TABLE pgcontext_dense_typmod_rejects (
            embedding vector(3) NOT NULL
        )",
    )
    .expect("dense typmod rejection table creation failed");

    assert_vector_typmod_statement_failure(
        "INSERT INTO pgcontext_dense_typmod_rejects (embedding)
         VALUES ('[1,2]'::vector)",
        "22023",
        "dimension mismatch: vector typmod requires 3 dimensions, value has 2",
        "dense typmod dimension enforcement",
    );
}

#[pg_test]
fn pgvector_variant_typmods_accept_matching_dimensions() {
    Spi::run(
        "CREATE TEMP TABLE pgcontext_variant_typmod_items (
            half_text halfvec(3) NOT NULL,
            half_real halfvec(3) NOT NULL,
            half_integer halfvec(3) NOT NULL,
            half_double halfvec(3) NOT NULL,
            sparse_text sparsevec(5) NOT NULL,
            sparse_real sparsevec(5) NOT NULL,
            bit_text bitvec(3) NOT NULL,
            bit_bool bitvec(3) NOT NULL,
            bit_builtin bitvec(3) NOT NULL,
            bit_varbit bitvec(3) NOT NULL
        )",
    )
    .expect("variant typmod table creation failed");
    Spi::run(
        "INSERT INTO pgcontext_variant_typmod_items (
            half_text,
            half_real,
            half_integer,
            half_double,
            sparse_text,
            sparse_real,
            bit_text,
            bit_bool,
            bit_builtin,
            bit_varbit
        )
        VALUES (
            pgcontext.halfvec('[1,2,3]'),
            ARRAY[1,2,3]::real[]::halfvec,
            ARRAY[1,2,3]::integer[]::halfvec,
            ARRAY[1,2,3]::double precision[]::halfvec,
            pgcontext.sparsevec('{1:1,5:2}/5'),
            ARRAY[1,0,0,0,2]::real[]::sparsevec,
            pgcontext.bitvec('101'),
            ARRAY[true,false,true]::boolean[]::bitvec,
            B'101'::bit(3)::bitvec,
            B'101'::bit varying::bitvec
        )",
    )
    .expect("variant typmod row insert failed");

    let details = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT
                    (SELECT format_type(atttypid, atttypmod)
                       FROM pg_catalog.pg_attribute
                      WHERE attrelid = 'pg_temp.pgcontext_variant_typmod_items'::regclass
                        AND attname = 'half_text') AS half_type,
                    (SELECT format_type(atttypid, atttypmod)
                      FROM pg_catalog.pg_attribute
                      WHERE attrelid = 'pg_temp.pgcontext_variant_typmod_items'::regclass
                        AND attname = 'sparse_text') AS sparse_type,
                    (SELECT format_type(atttypid, atttypmod)
                       FROM pg_catalog.pg_attribute
                      WHERE attrelid = 'pg_temp.pgcontext_variant_typmod_items'::regclass
                        AND attname = 'bit_text') AS bit_type,
                    pgcontext.halfvec_dims(half_text),
                    pgcontext.halfvec_dims(half_real),
                    pgcontext.halfvec_dims(half_integer),
                    pgcontext.halfvec_dims(half_double),
                    pgcontext.sparsevec_dims(sparse_text),
                    pgcontext.sparsevec_dims(sparse_real),
                    pgcontext.bitvec_dims(bit_text),
                    pgcontext.bitvec_dims(bit_bool),
                    pgcontext.bitvec_dims(bit_builtin),
                    pgcontext.bitvec_dims(bit_varbit),
                    sparse_real::text,
                    bit_text::text,
                    bit_bool::text,
                    bit_builtin::text,
                    bit_varbit::text
                   FROM pgcontext_variant_typmod_items",
                None,
                &[],
            )
            .expect("variant typmod metadata query failed");

        let row = result.first();
        Ok::<_, spi::Error>(VariantTypmodDetails {
            half_type: row.get::<String>(1)?.unwrap_or_default(),
            sparse_type: row.get::<String>(2)?.unwrap_or_default(),
            bit_type: row.get::<String>(3)?.unwrap_or_default(),
            half_text_dims: row.get::<i32>(4)?.unwrap_or_default(),
            half_real_dims: row.get::<i32>(5)?.unwrap_or_default(),
            half_integer_dims: row.get::<i32>(6)?.unwrap_or_default(),
            half_double_dims: row.get::<i32>(7)?.unwrap_or_default(),
            sparse_text_dims: row.get::<i32>(8)?.unwrap_or_default(),
            sparse_real_dims: row.get::<i32>(9)?.unwrap_or_default(),
            bit_text_dims: row.get::<i32>(10)?.unwrap_or_default(),
            bit_bool_dims: row.get::<i32>(11)?.unwrap_or_default(),
            bit_builtin_dims: row.get::<i32>(12)?.unwrap_or_default(),
            bit_varbit_dims: row.get::<i32>(13)?.unwrap_or_default(),
            sparse_real_text: row.get::<String>(14)?.unwrap_or_default(),
            bit_text: row.get::<String>(15)?.unwrap_or_default(),
            bit_bool_text: row.get::<String>(16)?.unwrap_or_default(),
            bit_builtin_text: row.get::<String>(17)?.unwrap_or_default(),
            bit_varbit_text: row.get::<String>(18)?.unwrap_or_default(),
        })
    })
    .expect("variant typmod metadata rows failed");

    assert_eq!(
        details,
        VariantTypmodDetails {
            half_type: "halfvec(3)".to_owned(),
            sparse_type: "sparsevec(5)".to_owned(),
            bit_type: "bitvec(3)".to_owned(),
            half_text_dims: 3,
            half_real_dims: 3,
            half_integer_dims: 3,
            half_double_dims: 3,
            sparse_text_dims: 5,
            sparse_real_dims: 5,
            bit_text_dims: 3,
            bit_bool_dims: 3,
            bit_builtin_dims: 3,
            bit_varbit_dims: 3,
            sparse_real_text: "{1:1,5:2}/5".to_owned(),
            bit_text: "101".to_owned(),
            bit_bool_text: "101".to_owned(),
            bit_builtin_text: "101".to_owned(),
            bit_varbit_text: "101".to_owned(),
        }
    );
}

#[pg_test]
fn pgvector_variant_typmods_reject_dimension_mismatches_with_sqlstates() {
    Spi::run(
        "CREATE TEMP TABLE pgcontext_variant_typmod_rejects (
            half_value halfvec(3) NOT NULL,
            sparse_value sparsevec(5) NOT NULL,
            bit_value bitvec(3) NOT NULL
        )",
    )
    .expect("variant typmod rejection table creation failed");

    let cases = [
        (
            "INSERT INTO pgcontext_variant_typmod_rejects (half_value, sparse_value)
             VALUES (pgcontext.halfvec('[1,2]'), pgcontext.sparsevec('{}/5'))",
            "22023",
            "dimension mismatch: halfvec typmod requires 3 dimensions, value has 2",
        ),
        (
            "INSERT INTO pgcontext_variant_typmod_rejects (half_value, sparse_value)
             VALUES (pgcontext.halfvec('[1,2,3,4]'), pgcontext.sparsevec('{}/5'))",
            "22023",
            "dimension mismatch: halfvec typmod requires 3 dimensions, value has 4",
        ),
        (
            "INSERT INTO pgcontext_variant_typmod_rejects (half_value, sparse_value)
             VALUES (ARRAY[1,2]::real[]::halfvec, pgcontext.sparsevec('{}/5'))",
            "22023",
            "dimension mismatch: halfvec typmod requires 3 dimensions, value has 2",
        ),
        (
            "INSERT INTO pgcontext_variant_typmod_rejects (half_value, sparse_value)
             VALUES (pgcontext.halfvec('[1,2,3]'), pgcontext.sparsevec('{1:1}/4'))",
            "22023",
            "dimension mismatch: sparsevec typmod requires 5 dimensions, value has 4",
        ),
        (
            "INSERT INTO pgcontext_variant_typmod_rejects (half_value, sparse_value)
             VALUES (pgcontext.halfvec('[1,2,3]'), ARRAY[1,2,3,4]::real[]::sparsevec)",
            "22023",
            "dimension mismatch: sparsevec typmod requires 5 dimensions, value has 4",
        ),
        (
            "INSERT INTO pgcontext_variant_typmod_rejects (half_value, sparse_value, bit_value)
             VALUES (pgcontext.halfvec('[1,2,3]'), pgcontext.sparsevec('{}/5'), pgcontext.bitvec('10'))",
            "22023",
            "dimension mismatch: bitvec typmod requires 3 dimensions, value has 2",
        ),
        (
            "INSERT INTO pgcontext_variant_typmod_rejects (half_value, sparse_value, bit_value)
             VALUES (pgcontext.halfvec('[1,2,3]'), pgcontext.sparsevec('{}/5'), ARRAY[true,false,true,false]::boolean[]::bitvec)",
            "22023",
            "dimension mismatch: bitvec typmod requires 3 dimensions, value has 4",
        ),
        (
            "INSERT INTO pgcontext_variant_typmod_rejects (half_value, sparse_value, bit_value)
             VALUES (pgcontext.halfvec('[1,2,3]'), pgcontext.sparsevec('{}/5'), B'10'::bit(2)::bitvec)",
            "22023",
            "dimension mismatch: bitvec typmod requires 3 dimensions, value has 2",
        ),
        (
            "INSERT INTO pgcontext_variant_typmod_rejects (half_value, sparse_value, bit_value)
             VALUES (pgcontext.halfvec('[1,2,3]'), pgcontext.sparsevec('{}/5'), B'10'::bit varying::bitvec)",
            "22023",
            "dimension mismatch: bitvec typmod requires 3 dimensions, value has 2",
        ),
    ];

    for (sql, sqlstate, message) in cases {
        assert_vector_typmod_statement_failure(
            sql,
            sqlstate,
            message,
            "variant typmod dimension enforcement",
        );
    }
}

#[pg_test]
fn pgvector_variant_typmods_reject_invalid_typmods_with_sqlstates() {
    let cases = [
        (
            "CREATE TEMP TABLE pgcontext_bad_halfvec_zero (value halfvec(0))",
            "22023",
            "halfvec typmod dimensions must be between 1 and 16000: 0",
        ),
        (
            "CREATE TEMP TABLE pgcontext_bad_halfvec_large (value halfvec(16001))",
            "22023",
            "halfvec typmod dimensions must be between 1 and 16000: 16001",
        ),
        (
            "CREATE TEMP TABLE pgcontext_bad_sparsevec_zero (value sparsevec(0))",
            "22023",
            "sparsevec typmod dimensions must be between 1 and 16000: 0",
        ),
        (
            "CREATE TEMP TABLE pgcontext_bad_sparsevec_large (value sparsevec(16001))",
            "22023",
            "sparsevec typmod dimensions must be between 1 and 16000: 16001",
        ),
        (
            "CREATE TEMP TABLE pgcontext_bad_bitvec_zero (value bitvec(0))",
            "22023",
            "bitvec typmod dimensions must be between 1 and 16000: 0",
        ),
        (
            "CREATE TEMP TABLE pgcontext_bad_bitvec_large (value bitvec(16001))",
            "22023",
            "bitvec typmod dimensions must be between 1 and 16000: 16001",
        ),
    ];

    for (sql, sqlstate, message) in cases {
        assert_vector_typmod_statement_failure(sql, sqlstate, message, "variant typmod parsing");
    }
}

fn assert_vector_typmod_statement_failure(
    sql: &str,
    sqlstate: &str,
    message: &str,
    context: &str,
) {
    let sql = sql.replace('\'', "''");
    let message = message.replace('\'', "''");
    Spi::run(&format!(
        r#"
        DO $$
        DECLARE
            actual_sqlstate text;
        BEGIN
            BEGIN
                EXECUTE '{sql}';
                RAISE EXCEPTION 'expected {context} failure';
            EXCEPTION WHEN OTHERS THEN
                GET STACKED DIAGNOSTICS actual_sqlstate = RETURNED_SQLSTATE;
                IF actual_sqlstate <> '{sqlstate}' THEN
                    RAISE EXCEPTION 'unexpected {context} SQLSTATE: %', actual_sqlstate;
                END IF;
                IF SQLERRM <> '{message}' THEN
                    RAISE EXCEPTION 'unexpected {context} error: %', SQLERRM;
                END IF;
            END;
        END $$;
        "#
    ))
    .expect("invalid vector typmod statement should raise expected error");
}

#[derive(Debug, PartialEq)]
struct VariantTypmodDetails {
    half_type: String,
    sparse_type: String,
    bit_type: String,
    half_text_dims: i32,
    half_real_dims: i32,
    half_integer_dims: i32,
    half_double_dims: i32,
    sparse_text_dims: i32,
    sparse_real_dims: i32,
    bit_text_dims: i32,
    bit_bool_dims: i32,
    bit_builtin_dims: i32,
    bit_varbit_dims: i32,
    sparse_real_text: String,
    bit_text: String,
    bit_bool_text: String,
    bit_builtin_text: String,
    bit_varbit_text: String,
}
