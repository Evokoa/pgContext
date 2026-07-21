#[pg_test]
fn pgvector_bitvec_bool_array_cast_cases() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT ARRAY[true,false,true,false]::boolean[]::bitvec::text,
                        pgcontext.bitvec('1010')::boolean[]",
                None,
                &[],
            )
            .expect("bitvec boolean array cast query failed");

        let row = result.first();
        Ok::<_, spi::Error>((
            row.get::<String>(1)?.unwrap_or_default(),
            row.get::<Vec<bool>>(2)?.unwrap_or_default(),
        ))
    })
    .expect("bitvec boolean array cast rows failed");

    assert_eq!(rows, ("1010".to_owned(), vec![true, false, true, false]));
}

#[pg_test]
fn pgvector_bitvec_postgres_bit_cast_cases() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT B'1010'::bit(4)::bitvec::text,
                        B'1010'::bit varying::bitvec::text,
                        pgcontext.bitvec('1010')::bit varying::text,
                        pgcontext.bitvec('1010')::bit(4)::text",
                None,
                &[],
            )
            .expect("bitvec PostgreSQL bit cast query failed");

        let row = result.first();
        Ok::<_, spi::Error>((
            row.get::<String>(1)?.unwrap_or_default(),
            row.get::<String>(2)?.unwrap_or_default(),
            row.get::<String>(3)?.unwrap_or_default(),
            row.get::<String>(4)?.unwrap_or_default(),
        ))
    })
    .expect("bitvec PostgreSQL bit cast rows failed");

    assert_eq!(
        rows,
        (
            "1010".to_owned(),
            "1010".to_owned(),
            "1010".to_owned(),
            "1010".to_owned()
        )
    );
}

#[pg_test]
fn pgvector_postgres_bit_distance_function_cases() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT pgcontext.hamming_distance(B'10101'::bit(5), B'11100'::bit(5)),
                        pgcontext.jaccard_distance(B'10101'::bit(5), B'11100'::bit(5)),
                        pgcontext.hamming_distance(B'10101'::bit varying, B'11100'::bit varying),
                        pgcontext.jaccard_distance(B'10101'::bit varying, B'11100'::bit varying)",
                None,
                &[],
            )
            .expect("PostgreSQL bit distance query failed");

        let row = result.first();
        Ok::<_, spi::Error>((
            row.get::<f64>(1)?.unwrap_or_default(),
            row.get::<f64>(2)?.unwrap_or_default(),
            row.get::<f64>(3)?.unwrap_or_default(),
            row.get::<f64>(4)?.unwrap_or_default(),
        ))
    })
    .expect("PostgreSQL bit distance rows failed");

    assert_eq!(rows.0, 2.0);
    assert!((rows.1 - 0.5).abs() < 0.000_001);
    assert_eq!(rows.2, 2.0);
    assert!((rows.3 - 0.5).abs() < 0.000_001);
}

#[pg_test]
fn pgvector_postgres_bit_distance_operator_cases() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT B'10101'::bit(5) OPERATOR(pgcontext.<~>) B'11100'::bit(5),
                        B'10101'::bit(5) OPERATOR(pgcontext.<%>) B'11100'::bit(5),
                        B'10101'::bit varying OPERATOR(pgcontext.<~>) B'11100'::bit varying,
                        B'10101'::bit varying OPERATOR(pgcontext.<%>) B'11100'::bit varying",
                None,
                &[],
            )
            .expect("PostgreSQL bit distance operator query failed");

        let row = result.first();
        Ok::<_, spi::Error>((
            row.get::<f64>(1)?.unwrap_or_default(),
            row.get::<f64>(2)?.unwrap_or_default(),
            row.get::<f64>(3)?.unwrap_or_default(),
            row.get::<f64>(4)?.unwrap_or_default(),
        ))
    })
    .expect("PostgreSQL bit distance operator rows failed");

    assert_eq!(rows.0, 2.0);
    assert!((rows.1 - 0.5).abs() < 0.000_001);
    assert_eq!(rows.2, 2.0);
    assert!((rows.3 - 0.5).abs() < 0.000_001);
}

#[pg_test]
fn pgvector_postgres_bit_distance_rejects_dimension_mismatch_with_sqlstate() {
    let cases = [
        (
            "SELECT pgcontext.hamming_distance(B'101'::bit(3), B'10'::bit(2))",
            "fixed-bit hamming mismatch",
        ),
        (
            "SELECT pgcontext.jaccard_distance(B'101'::bit(3), B'10'::bit(2))",
            "fixed-bit jaccard mismatch",
        ),
        (
            "SELECT pgcontext.hamming_distance(B'101'::bit varying, B'10'::bit varying)",
            "varbit hamming mismatch",
        ),
        (
            "SELECT pgcontext.jaccard_distance(B'101'::bit varying, B'10'::bit varying)",
            "varbit jaccard mismatch",
        ),
        (
            "SELECT B'101'::bit(3) OPERATOR(pgcontext.<~>) B'10'::bit(2)",
            "fixed-bit hamming operator mismatch",
        ),
        (
            "SELECT B'101'::bit(3) OPERATOR(pgcontext.<%>) B'10'::bit(2)",
            "fixed-bit jaccard operator mismatch",
        ),
        (
            "SELECT B'101'::bit varying OPERATOR(pgcontext.<~>) B'10'::bit varying",
            "varbit hamming operator mismatch",
        ),
        (
            "SELECT B'101'::bit varying OPERATOR(pgcontext.<%>) B'10'::bit varying",
            "varbit jaccard operator mismatch",
        ),
    ];

    for (sql, label) in cases {
        assert_vector_compat_sql_failure(
            sql,
            "22023",
            "dimension mismatch: left has 3 dimensions, right has 2",
            label,
        );
    }
}

#[pg_test]
fn pgvector_postgres_bit_distance_rejects_empty_varbit_with_sqlstate() {
    let cases = [
        (
            "SELECT pgcontext.hamming_distance(B''::bit varying, B'1'::bit varying)",
            "empty varbit hamming",
        ),
        (
            "SELECT pgcontext.jaccard_distance(B''::bit varying, B'1'::bit varying)",
            "empty varbit jaccard",
        ),
    ];

    for (sql, label) in cases {
        assert_vector_compat_sql_failure(
            sql,
            "22P02",
            "invalid vector: bit vectors must contain at least one bit",
            label,
        );
    }
}

#[pg_test]
fn pgvector_bitvec_bool_array_cast_rejects_invalid_inputs_with_sqlstates() {
    assert_vector_compat_sql_failure(
        "SELECT ARRAY[]::boolean[]::bitvec",
        "22P02",
        "invalid vector: bit vectors must contain at least one bit",
        "bitvec boolean-array cast",
    );
}

#[pg_test]
fn pgvector_bitvec_postgres_bit_cast_rejects_empty_with_sqlstate() {
    assert_vector_compat_sql_failure(
        "SELECT B''::bit varying::bitvec",
        "22P02",
        "invalid vector: bit vectors must contain at least one bit",
        "bitvec PostgreSQL bit cast",
    );
}

#[pg_test]
fn pgvector_bitvec_rejects_implicit_fixed_bit_assignment() {
    Spi::run(
        r#"
        DO $$
        DECLARE
            actual_sqlstate text;
        BEGIN
            CREATE TEMP TABLE bitvec_assignment_guard (bits bit(3));
            BEGIN
                INSERT INTO bitvec_assignment_guard VALUES (pgcontext.bitvec('1010'));
                RAISE EXCEPTION 'expected bitvec fixed-bit assignment failure';
            EXCEPTION WHEN OTHERS THEN
                GET STACKED DIAGNOSTICS actual_sqlstate = RETURNED_SQLSTATE;
                IF actual_sqlstate <> '42804' THEN
                    RAISE EXCEPTION 'unexpected bitvec fixed-bit assignment SQLSTATE: %',
                        actual_sqlstate;
                END IF;
                IF SQLERRM <> 'column "bits" is of type bit but expression is of type bitvec' THEN
                    RAISE EXCEPTION 'unexpected bitvec fixed-bit assignment error: %', SQLERRM;
                END IF;
            END;
        END $$;
        "#,
    )
    .expect("bitvec fixed-bit assignment should raise expected error");
}

#[pg_test]
fn pgvector_bitvec_aggregate_cases() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT pgcontext.bit_or(value)::text,
                        pgcontext.bit_and(value)::text
                   FROM (VALUES
                        (pgcontext.bitvec('1010')),
                        (pgcontext.bitvec('1100')),
                        (NULL::bitvec)
                   ) AS items(value)",
                None,
                &[],
            )
            .expect("bitvec aggregate query failed");

        let row = result.first();
        Ok::<_, spi::Error>((
            row.get::<String>(1)?.unwrap_or_default(),
            row.get::<String>(2)?.unwrap_or_default(),
        ))
    })
    .expect("bitvec aggregate rows failed");

    assert_eq!(rows, ("1110".to_owned(), "1000".to_owned()));
}

#[pg_test]
fn pgvector_bitvec_aggregates_return_null_for_empty_input() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT pgcontext.bit_or(value)::text, pgcontext.bit_and(value)::text
                   FROM (SELECT NULL::bitvec AS value WHERE false) AS empty",
                None,
                &[],
            )
            .expect("empty bitvec aggregate query failed");

        let row = result.first();
        Ok::<_, spi::Error>((row.get::<String>(1)?.is_none(), row.get::<String>(2)?.is_none()))
    })
    .expect("empty bitvec aggregate rows failed");

    assert_eq!(rows, (true, true));
}

#[pg_test]
fn pgvector_bitvec_aggregates_reject_dimension_mismatch_with_sqlstate() {
    assert_vector_compat_sql_failure(
        "SELECT pgcontext.bit_or(value)
           FROM (VALUES (pgcontext.bitvec('1010')), (pgcontext.bitvec('11'))) AS items(value)",
        "22023",
        "dimension mismatch: left has 4 dimensions, right has 2",
        "bitvec aggregate dimension mismatch",
    );
}
