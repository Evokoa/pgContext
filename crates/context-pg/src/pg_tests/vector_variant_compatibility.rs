#[pg_test]
fn pgvector_variant_input_output_and_dims_cases() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT pgcontext.halfvec('[1, 2, 3]')::text,
                        pgcontext.halfvec_dims(pgcontext.halfvec('[1,2,3]')),
                        pgcontext.sparsevec('{3:2,1:1.5}/5')::text,
                        pgcontext.sparsevec_dims(pgcontext.sparsevec('{1:1.5,3:2}/5')),
                        pgcontext.bitvec('10101')::text,
                        pgcontext.bitvec_dims(pgcontext.bitvec('10101'))",
                None,
                &[],
            )
            .expect("variant vector input/output query failed");

        let row = result.first();
        Ok::<_, spi::Error>((
            row.get::<String>(1)?.unwrap_or_default(),
            row.get::<i32>(2)?.unwrap_or_default(),
            row.get::<String>(3)?.unwrap_or_default(),
            row.get::<i32>(4)?.unwrap_or_default(),
            row.get::<String>(5)?.unwrap_or_default(),
            row.get::<i32>(6)?.unwrap_or_default(),
        ))
    })
    .expect("variant vector input/output rows failed");

    assert_eq!(
        rows,
        (
            "[1,2,3]".to_owned(),
            3,
            "{1:1.5,3:2}/5".to_owned(),
            5,
            "10101".to_owned(),
            5,
        )
    );
}

#[pg_test]
fn pgvector_variant_distance_function_cases() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT pgcontext.halfvec_l2_distance(pgcontext.halfvec('[1,2,3]'), pgcontext.halfvec('[4,6,3]')),
                        pgcontext.halfvec_inner_product(pgcontext.halfvec('[1,2,3]'), pgcontext.halfvec('[4,6,3]')),
                        pgcontext.halfvec_negative_inner_product(pgcontext.halfvec('[1,2,3]'), pgcontext.halfvec('[4,6,3]')),
                        pgcontext.halfvec_cosine_distance(pgcontext.halfvec('[1,2,3]'), pgcontext.halfvec('[4,6,3]')),
                        pgcontext.halfvec_l1_distance(pgcontext.halfvec('[1,2,3]'), pgcontext.halfvec('[4,6,3]')),
                        pgcontext.sparsevec_l2_distance(pgcontext.sparsevec('{1:1,3:2}/4'), pgcontext.sparsevec('{1:4,2:5}/4')),
                        pgcontext.sparsevec_inner_product(pgcontext.sparsevec('{1:1,3:2}/4'), pgcontext.sparsevec('{1:4,2:5}/4')),
                        pgcontext.sparsevec_negative_inner_product(pgcontext.sparsevec('{1:1,3:2}/4'), pgcontext.sparsevec('{1:4,2:5}/4')),
                        pgcontext.sparsevec_cosine_distance(pgcontext.sparsevec('{1:1,3:2}/4'), pgcontext.sparsevec('{1:4,2:5}/4')),
                        pgcontext.sparsevec_l1_distance(pgcontext.sparsevec('{1:1,3:2}/4'), pgcontext.sparsevec('{1:4,2:5}/4')),
                        pgcontext.bitvec_hamming_distance(pgcontext.bitvec('10101'), pgcontext.bitvec('11100')),
                        pgcontext.bitvec_jaccard_distance(pgcontext.bitvec('10101'), pgcontext.bitvec('11100'))",
                None,
                &[],
            )
            .expect("variant vector distance query failed");

        let row = result.first();
        Ok::<_, spi::Error>((
            row.get::<f32>(1)?.unwrap_or_default(),
            row.get::<f32>(2)?.unwrap_or_default(),
            row.get::<f32>(3)?.unwrap_or_default(),
            row.get::<f32>(4)?.unwrap_or_default(),
            row.get::<f32>(5)?.unwrap_or_default(),
            row.get::<f32>(6)?.unwrap_or_default(),
            row.get::<f32>(7)?.unwrap_or_default(),
            row.get::<f32>(8)?.unwrap_or_default(),
            row.get::<f32>(9)?.unwrap_or_default(),
            row.get::<f32>(10)?.unwrap_or_default(),
            row.get::<i32>(11)?.unwrap_or_default(),
            row.get::<f64>(12)?.unwrap_or_default(),
        ))
    })
    .expect("variant vector distance rows failed");

    assert_eq!(rows.0, 5.0);
    assert_eq!(rows.1, 25.0);
    assert_eq!(rows.2, -25.0);
    assert!((rows.3 - 0.144_517_61).abs() < 0.000_001);
    assert_eq!(rows.4, 7.0);
    assert!((rows.5 - 6.164_414).abs() < 0.000_001);
    assert_eq!(rows.6, 4.0);
    assert_eq!(rows.7, -4.0);
    assert!((rows.8 - 0.720_627_9).abs() < 0.000_001);
    assert_eq!(rows.9, 10.0);
    assert_eq!(rows.10, 2);
    assert!((rows.11 - 0.5).abs() < 0.000_001);
}

#[pg_test]
fn pgvector_variant_constructors_reject_invalid_input() {
    let cases = [
        (
            "SELECT pgcontext.halfvec('[]')",
            "22P02",
            "invalid vector: halfvec values must contain at least one value",
        ),
        (
            "SELECT pgcontext.sparsevec('{1:2,1:3}/3')",
            "22P02",
            "invalid vector: sparsevec duplicate index: 1",
        ),
        (
            "SELECT pgcontext.bitvec('102')",
            "22P02",
            "invalid vector: invalid bit at position 2: 2",
        ),
        (
            "SELECT pgcontext.bitvec_hamming_distance(pgcontext.bitvec('101'), pgcontext.bitvec('10'))",
            "22023",
            "dimension mismatch: left has 3 dimensions, right has 2",
        ),
    ];

    for (sql, sqlstate, message) in cases {
        assert_vector_compat_sql_failure(sql, sqlstate, message, "vector variant constructor");
    }
}

#[pg_test]
fn pgvector_sparsevec_array_constructor_and_accessors_canonicalize_entries() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT pgcontext.sparsevec_from_arrays(
                            ARRAY[3,1]::integer[],
                            ARRAY[2,1.5]::real[],
                            5
                        )::text,
                        pgcontext.sparsevec_dims(
                            pgcontext.sparsevec_from_arrays(ARRAY[3,1], ARRAY[2,1.5]::real[], 5)
                        ),
                        pgcontext.sparsevec_indices(
                            pgcontext.sparsevec_from_arrays(ARRAY[3,1], ARRAY[2,1.5]::real[], 5)
                        ),
                        pgcontext.sparsevec_values(
                            pgcontext.sparsevec_from_arrays(ARRAY[3,1], ARRAY[2,1.5]::real[], 5)
                        )",
                None,
                &[],
            )
            .expect("sparsevec array constructor query failed");

        let row = result.first();
        Ok::<_, spi::Error>((
            row.get::<String>(1)?.unwrap_or_default(),
            row.get::<i32>(2)?.unwrap_or_default(),
            row.get::<Vec<i32>>(3)?.unwrap_or_default(),
            row.get::<Vec<f32>>(4)?.unwrap_or_default(),
        ))
    })
    .expect("sparsevec array constructor rows failed");

    assert_eq!(
        rows,
        (
            "{1:1.5,3:2}/5".to_owned(),
            5,
            vec![1, 3],
            vec![1.5, 2.0]
        )
    );
}

#[pg_test]
fn pgvector_sparsevec_array_constructor_rejects_invalid_inputs_with_sqlstates() {
    let cases = [
        (
            "SELECT pgcontext.sparsevec_from_arrays(ARRAY[1,2]::integer[], ARRAY[1]::real[], 3)",
            "22023",
            "sparsevec indices and values must have the same length: got 2 indices and 1 values",
        ),
        (
            "SELECT pgcontext.sparsevec_from_arrays(ARRAY[-1]::integer[], ARRAY[1]::real[], 3)",
            "22023",
            "invalid sparsevec index: -1",
        ),
        (
            "SELECT pgcontext.sparsevec_from_arrays(ARRAY[1]::integer[], ARRAY[1]::real[], -3)",
            "22023",
            "invalid sparsevec dimensions: -3",
        ),
        (
            "SELECT pgcontext.sparsevec_from_arrays(ARRAY[1,1]::integer[], ARRAY[1,2]::real[], 3)",
            "22P02",
            "invalid vector: sparsevec duplicate index: 1",
        ),
        (
            "SELECT pgcontext.sparsevec_from_arrays(ARRAY[4]::integer[], ARRAY[1]::real[], 3)",
            "22P02",
            "invalid vector: sparsevec index 4 exceeds dimensions 3",
        ),
    ];

    for (sql, sqlstate, message) in cases {
        assert_vector_compat_sql_failure(sql, sqlstate, message, "sparsevec array constructor");
    }
}

#[pg_test]
fn pgvector_sparsevec_real_array_cast_cases() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT ARRAY[0,1.5,0,-2]::real[]::sparsevec::text,
                        ARRAY[0,0,0]::real[]::sparsevec::text,
                        pgcontext.sparsevec('{2:1.5,4:-2}/4')::real[]",
                None,
                &[],
            )
            .expect("sparsevec real array cast query failed");

        let row = result.first();
        Ok::<_, spi::Error>((
            row.get::<String>(1)?.unwrap_or_default(),
            row.get::<String>(2)?.unwrap_or_default(),
            row.get::<Vec<f32>>(3)?.unwrap_or_default(),
        ))
    })
    .expect("sparsevec real array cast rows failed");

    assert_eq!(
        rows,
        (
            "{2:1.5,4:-2}/4".to_owned(),
            "{}/3".to_owned(),
            vec![0.0, 1.5, 0.0, -2.0]
        )
    );
}

#[pg_test]
fn pgvector_sparsevec_real_array_cast_rejects_invalid_inputs_with_sqlstates() {
    let cases = [
        (
            "SELECT ARRAY[]::real[]::sparsevec",
            "22P02",
            "invalid vector: sparsevec dimensions must be greater than zero",
        ),
        (
            "SELECT ARRAY['NaN'::real]::sparsevec",
            "22P02",
            "invalid vector: sparsevec value at index 1 is not finite: NaN",
        ),
    ];

    for (sql, sqlstate, message) in cases {
        assert_vector_compat_sql_failure(sql, sqlstate, message, "sparsevec real-array cast");
    }
}

#[pg_test]
fn pgvector_sparsevec_dense_vector_cast_cases() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT '[0,1.5,0,-2]'::vector::sparsevec::text,
                        pgcontext.sparsevec('{2:1.5,4:-2}/4')::vector::text,
                        '[0,0,0]'::vector::sparsevec::text",
                None,
                &[],
            )
            .expect("sparsevec dense vector cast query failed");

        let row = result.first();
        Ok::<_, spi::Error>((
            row.get::<String>(1)?.unwrap_or_default(),
            row.get::<String>(2)?.unwrap_or_default(),
            row.get::<String>(3)?.unwrap_or_default(),
        ))
    })
    .expect("sparsevec dense vector cast rows failed");

    assert_eq!(
        rows,
        (
            "{2:1.5,4:-2}/4".to_owned(),
            "[0,1.5,0,-2]".to_owned(),
            "{}/3".to_owned(),
        )
    );
}

#[pg_test]
fn pgvector_sparsevec_dense_vector_casts_enforce_typmods() {
    Spi::run(
        "CREATE TEMP TABLE pgcontext_sparsevec_dense_cast_typmod (
            value sparsevec(4) NOT NULL
        )",
    )
    .expect("sparsevec dense cast typmod table creation failed");

    Spi::run(
        "INSERT INTO pgcontext_sparsevec_dense_cast_typmod (value)
         VALUES ('[0,1.5,0,-2]'::vector::sparsevec)",
    )
    .expect("sparsevec dense cast typmod insert failed");

    let stored = Spi::get_one::<String>(
        "SELECT value::text FROM pgcontext_sparsevec_dense_cast_typmod",
    )
    .expect("sparsevec dense cast typmod select failed")
    .unwrap_or_default();

    assert_eq!(stored, "{2:1.5,4:-2}/4");

    assert_vector_compat_sql_failure(
        "SELECT '[1,2,3]'::vector::sparsevec::sparsevec(4)",
        "22023",
        "dimension mismatch: sparsevec typmod requires 4 dimensions, value has 3",
        "sparsevec dense-vector cast typmod mismatch",
    );
}

#[pg_test]
fn pgvector_sparsevec_dense_vector_cast_rejects_invalid_inputs_with_sqlstates() {
    let cases = [
        (
            "SELECT '[]'::vector::sparsevec",
            "22P02",
            "invalid vector: dense vectors must contain at least one value",
        ),
        (
            "SELECT '[NaN]'::vector::sparsevec",
            "22P02",
            "invalid vector: value at dimension 0 is not finite: NaN",
        ),
        (
            "SELECT pgcontext.sparsevec('{1:NaN}/1')::vector",
            "22P02",
            "invalid vector: sparsevec value at index 1 is not finite: NaN",
        ),
    ];

    for (sql, sqlstate, message) in cases {
        assert_vector_compat_sql_failure(sql, sqlstate, message, "sparsevec dense-vector cast");
    }
}

#[pg_test]
fn pgvector_halfvec_array_cast_cases() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT ARRAY[1,2,3]::real[]::halfvec::text,
                        ARRAY[1,2,3]::integer[]::halfvec::text,
                        ARRAY[1,2,3]::double precision[]::halfvec::text,
                        pgcontext.halfvec('[1,2,3]')::real[]",
                None,
                &[],
            )
            .expect("halfvec array cast query failed");

        let row = result.first();
        Ok::<_, spi::Error>((
            row.get::<String>(1)?.unwrap_or_default(),
            row.get::<String>(2)?.unwrap_or_default(),
            row.get::<String>(3)?.unwrap_or_default(),
            row.get::<Vec<f32>>(4)?.unwrap_or_default(),
        ))
    })
    .expect("halfvec array cast rows failed");

    assert_eq!(
        rows,
        (
            "[1,2,3]".to_owned(),
            "[1,2,3]".to_owned(),
            "[1,2,3]".to_owned(),
            vec![1.0, 2.0, 3.0]
        )
    );
}

#[pg_test]
fn pgvector_halfvec_array_cast_rejects_invalid_inputs_with_sqlstates() {
    let cases = [
        (
            "SELECT ARRAY[]::real[]::halfvec",
            "22P02",
            "invalid vector: halfvec values must contain at least one value",
        ),
        (
            "SELECT ARRAY['NaN'::real]::halfvec",
            "22P02",
            "invalid vector: halfvec value at dimension 0 is not finite: NaN",
        ),
        (
            "SELECT ARRAY[70000]::real[]::halfvec",
            "22P02",
            "invalid vector: halfvec value at dimension 0 exceeds finite half precision range: 70000",
        ),
    ];

    for (sql, sqlstate, message) in cases {
        assert_vector_compat_sql_failure(sql, sqlstate, message, "halfvec array cast");
    }
}

#[pg_test]
fn pgvector_variant_distance_operator_cases() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT pgcontext.halfvec('[1,2,3]') OPERATOR(pgcontext.<->) pgcontext.halfvec('[4,6,3]'),
                        pgcontext.halfvec('[1,2,3]') OPERATOR(pgcontext.<#>) pgcontext.halfvec('[4,6,3]'),
                        pgcontext.halfvec('[1,2,3]') OPERATOR(pgcontext.<=>) pgcontext.halfvec('[4,6,3]'),
                        pgcontext.halfvec('[1,2,3]') OPERATOR(pgcontext.<+>) pgcontext.halfvec('[4,6,3]'),
                        pgcontext.sparsevec('{1:1,3:2}/4') OPERATOR(pgcontext.<->) pgcontext.sparsevec('{1:4,2:5}/4'),
                        pgcontext.sparsevec('{1:1,3:2}/4') OPERATOR(pgcontext.<#>) pgcontext.sparsevec('{1:4,2:5}/4'),
                        pgcontext.sparsevec('{1:1,3:2}/4') OPERATOR(pgcontext.<=>) pgcontext.sparsevec('{1:4,2:5}/4'),
                        pgcontext.sparsevec('{1:1,3:2}/4') OPERATOR(pgcontext.<+>) pgcontext.sparsevec('{1:4,2:5}/4'),
                        pgcontext.bitvec('10101') OPERATOR(pgcontext.<~>) pgcontext.bitvec('11100'),
                        pgcontext.bitvec('10101') OPERATOR(pgcontext.<%>) pgcontext.bitvec('11100')",
                None,
                &[],
            )
            .expect("variant vector distance operator query failed");

        let row = result.first();
        Ok::<_, spi::Error>((
            row.get::<f32>(1)?.unwrap_or_default(),
            row.get::<f32>(2)?.unwrap_or_default(),
            row.get::<f32>(3)?.unwrap_or_default(),
            row.get::<f32>(4)?.unwrap_or_default(),
            row.get::<f32>(5)?.unwrap_or_default(),
            row.get::<f32>(6)?.unwrap_or_default(),
            row.get::<f32>(7)?.unwrap_or_default(),
            row.get::<f32>(8)?.unwrap_or_default(),
            row.get::<i32>(9)?.unwrap_or_default(),
            row.get::<f64>(10)?.unwrap_or_default(),
        ))
    })
    .expect("variant vector distance operator rows failed");

    assert_eq!(rows.0, 5.0);
    assert_eq!(rows.1, -25.0);
    assert!((rows.2 - 0.144_517_61).abs() < 0.000_001);
    assert_eq!(rows.3, 7.0);
    assert!((rows.4 - 6.164_414).abs() < 0.000_001);
    assert_eq!(rows.5, -4.0);
    assert!((rows.6 - 0.720_627_9).abs() < 0.000_001);
    assert_eq!(rows.7, 10.0);
    assert_eq!(rows.8, 2);
    assert!((rows.9 - 0.5).abs() < 0.000_001);
}

#[pg_test]
fn pgvector_sparsevec_cosine_rejects_zero_vectors_with_sqlstates() {
    let cases = [
        (
            "SELECT pgcontext.sparsevec_cosine_distance(
                pgcontext.sparsevec_from_arrays(ARRAY[]::integer[], ARRAY[]::real[], 4),
                pgcontext.sparsevec_from_arrays(ARRAY[1]::integer[], ARRAY[1]::real[], 4)
            )",
            "sparsevec cosine function zero vector",
        ),
        (
            "SELECT pgcontext.sparsevec_from_arrays(ARRAY[]::integer[], ARRAY[]::real[], 4)
                OPERATOR(pgcontext.<=>)
                pgcontext.sparsevec_from_arrays(ARRAY[1]::integer[], ARRAY[1]::real[], 4)",
            "sparsevec cosine operator zero vector",
        ),
    ];

    for (sql, context) in cases {
        assert_vector_compat_sql_failure(
            sql,
            "22P02",
            "invalid vector: sparse cosine distance is undefined for zero vectors",
            context,
        );
    }
}

#[pg_test]
fn pgvector_variant_distance_operators_support_order_by() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT id
                   FROM (VALUES
                        (30, pgcontext.halfvec('[2,0]')),
                        (10, pgcontext.halfvec('[1,0]')),
                        (20, pgcontext.halfvec('[0,1]'))
                   ) AS items(id, embedding)
                  ORDER BY embedding OPERATOR(pgcontext.<->) pgcontext.halfvec('[0,0]'), id",
                None,
                &[],
            )
            .expect("halfvec operator ordering query failed");

        let mut rows = Vec::new();
        for row in result {
            rows.push(row.get::<i32>(1)?.unwrap_or_default());
        }
        Ok::<_, spi::Error>(rows)
    })
    .expect("halfvec operator ordering rows failed");

    assert_eq!(rows, vec![10, 20, 30]);
}

#[pg_test]
fn pgvector_halfvec_aggregate_cases() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT pgcontext.sum(value)::text,
                        pgcontext.avg(value)::text
                   FROM (VALUES (pgcontext.halfvec('[1,2]')), (pgcontext.halfvec('[3,4]'))) AS items(value)",
                None,
                &[],
            )
            .expect("halfvec aggregate query failed");

        let row = result.first();
        Ok::<_, spi::Error>((
            row.get::<String>(1)?.unwrap_or_default(),
            row.get::<String>(2)?.unwrap_or_default(),
        ))
    })
    .expect("halfvec aggregate rows failed");

    assert_eq!(rows, ("[4,6]".to_owned(), "[2,3]".to_owned()));
}

#[pg_test]
fn pgvector_halfvec_aggregates_return_null_for_empty_input() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT pgcontext.sum(value)::text, pgcontext.avg(value)::text
                   FROM (SELECT NULL::halfvec AS value WHERE false) AS empty",
                None,
                &[],
            )
            .expect("empty halfvec aggregate query failed");

        let row = result.first();
        Ok::<_, spi::Error>((row.get::<String>(1)?.is_none(), row.get::<String>(2)?.is_none()))
    })
    .expect("empty halfvec aggregate rows failed");

    assert_eq!(rows, (true, true));
}

#[pg_test]
fn pgvector_halfvec_aggregates_reject_dimension_mismatch_with_sqlstate() {
    assert_vector_compat_sql_failure(
        "SELECT pgcontext.sum(value)
           FROM (VALUES (pgcontext.halfvec('[1,2]')), (pgcontext.halfvec('[3]'))) AS items(value)",
        "22023",
        "dimension mismatch: left has 2 dimensions, right has 1",
        "halfvec aggregate dimension mismatch",
    );
}

#[pg_test]
fn pgvector_halfvec_sum_rejects_half_precision_overflow_with_sqlstate() {
    assert_vector_compat_sql_failure(
        "SELECT pgcontext.sum(value)
           FROM (VALUES (pgcontext.halfvec('[35000]')), (pgcontext.halfvec('[35000]'))) AS items(value)",
        "22P02",
        // 35000 is not representable in half precision: the f16 spacing in
        // this range is 32, and ties-to-even rounds it to 35008, so the sum
        // that overflows is 70016 rather than the decimal 70000.
        "invalid vector: halfvec value at dimension 0 exceeds finite half precision range: 70016",
        "halfvec aggregate overflow",
    );
}

#[pg_test]
fn pgvector_sparsevec_aggregate_cases() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT pgcontext.sum(value)::text,
                        pgcontext.avg(value)::text
                   FROM (VALUES
                        (pgcontext.sparsevec('{1:1,3:2}/4')),
                        (pgcontext.sparsevec('{2:3,3:-2}/4'))
                   ) AS items(value)",
                None,
                &[],
            )
            .expect("sparsevec aggregate query failed");

        let row = result.first();
        Ok::<_, spi::Error>((
            row.get::<String>(1)?.unwrap_or_default(),
            row.get::<String>(2)?.unwrap_or_default(),
        ))
    })
    .expect("sparsevec aggregate rows failed");

    assert_eq!(
        rows,
        ("{1:1,2:3}/4".to_owned(), "{1:0.5,2:1.5}/4".to_owned())
    );
}

#[pg_test]
fn pgvector_sparsevec_aggregates_return_null_for_empty_input() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT pgcontext.sum(value)::text, pgcontext.avg(value)::text
                   FROM (SELECT NULL::sparsevec AS value WHERE false) AS empty",
                None,
                &[],
            )
            .expect("empty sparsevec aggregate query failed");

        let row = result.first();
        Ok::<_, spi::Error>((row.get::<String>(1)?.is_none(), row.get::<String>(2)?.is_none()))
    })
    .expect("empty sparsevec aggregate rows failed");

    assert_eq!(rows, (true, true));
}

#[pg_test]
fn pgvector_sparsevec_aggregates_reject_dimension_mismatch_with_sqlstate() {
    assert_vector_compat_sql_failure(
        "SELECT pgcontext.sum(value)
           FROM (VALUES
                (pgcontext.sparsevec('{1:1}/4')),
                (pgcontext.sparsevec('{1:1}/3'))
           ) AS items(value)",
        "22023",
        "dimension mismatch: left has 4 dimensions, right has 3",
        "sparsevec aggregate dimension mismatch",
    );
}

#[pg_test]
fn pgvector_variant_btree_ordering_cases() {
    Spi::run(
        "CREATE TEMP TABLE pgcontext_variant_btree_items (
            id integer PRIMARY KEY,
            half_value halfvec NOT NULL,
            sparse_value sparsevec NOT NULL,
            bit_value bitvec NOT NULL
        )",
    )
    .expect("variant vector btree table creation failed");
    Spi::run(
        "INSERT INTO pgcontext_variant_btree_items (id, half_value, sparse_value, bit_value)
         VALUES
            (40, pgcontext.halfvec('[1,2]'), pgcontext.sparsevec('{2:1}/4'), pgcontext.bitvec('11')),
            (20, pgcontext.halfvec('[1,1]'), pgcontext.sparsevec('{1:1}/4'), pgcontext.bitvec('01')),
            (30, pgcontext.halfvec('[1,1,0]'), pgcontext.sparsevec('{1:1,2:1}/4'), pgcontext.bitvec('10')),
            (10, pgcontext.halfvec('[0,9]'), pgcontext.sparsevec('{}/4'), pgcontext.bitvec('0'))",
    )
    .expect("variant vector btree rows insert failed");
    Spi::run(
        "CREATE INDEX pgcontext_variant_btree_half_idx
            ON pgcontext_variant_btree_items USING btree (half_value)",
    )
    .expect("halfvec btree index creation failed");
    Spi::run(
        "CREATE INDEX pgcontext_variant_btree_sparse_idx
            ON pgcontext_variant_btree_items USING btree (sparse_value)",
    )
    .expect("sparsevec btree index creation failed");
    Spi::run(
        "CREATE INDEX pgcontext_variant_btree_bit_idx
            ON pgcontext_variant_btree_items USING btree (bit_value)",
    )
    .expect("bitvec btree index creation failed");

    let details = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT
                    (
                        SELECT array_agg(opcname::text ORDER BY opcname)
                          FROM pg_catalog.pg_opclass opclass
                          JOIN pg_catalog.pg_am am
                            ON am.oid = opclass.opcmethod
                         WHERE opcname IN ('halfvec_ops', 'sparsevec_ops', 'bitvec_ops')
                           AND am.amname = 'btree'
                           AND opcdefault
                    ) AS default_opclasses,
                    pgcontext.halfvec_cmp(pgcontext.halfvec('[1,1]'), pgcontext.halfvec('[1,1,0]')) AS half_cmp_prefix,
                    pgcontext.sparsevec_cmp(pgcontext.sparsevec('{1:1}/4'), pgcontext.sparsevec('{1:1,2:1}/4')) AS sparse_cmp_prefix,
                    pgcontext.bitvec_cmp(pgcontext.bitvec('01'), pgcontext.bitvec('10')) AS bit_cmp_value,
                    (
                        SELECT array_agg(id ORDER BY half_value, id)
                          FROM pgcontext_variant_btree_items
                    ) AS half_order,
                    (
                        SELECT array_agg(id ORDER BY sparse_value, id)
                          FROM pgcontext_variant_btree_items
                    ) AS sparse_order,
                    (
                        SELECT array_agg(id ORDER BY bit_value, id)
                          FROM pgcontext_variant_btree_items
                    ) AS bit_order",
                None,
                &[],
            )
            .expect("variant vector btree metadata query failed");

        let row = result.first();
        Ok::<_, spi::Error>((
            row.get::<Vec<String>>(1)?.unwrap_or_default(),
            row.get::<i32>(2)?.unwrap_or_default(),
            row.get::<i32>(3)?.unwrap_or_default(),
            row.get::<i32>(4)?.unwrap_or_default(),
            row.get::<Vec<i32>>(5)?.unwrap_or_default(),
            row.get::<Vec<i32>>(6)?.unwrap_or_default(),
            row.get::<Vec<i32>>(7)?.unwrap_or_default(),
        ))
    })
    .expect("variant vector btree metadata rows failed");

    assert_eq!(
        details,
        (
            vec![
                "bitvec_ops".to_owned(),
                "halfvec_ops".to_owned(),
                "sparsevec_ops".to_owned()
            ],
            -1,
            -1,
            -1,
            vec![10, 20, 30, 40],
            vec![10, 20, 30, 40],
            vec![10, 20, 30, 40],
        )
    );
}

#[pg_test]
fn pgvector_variant_hnsw_indexes_use_dense_storage_and_exact_order() {
    let builtin_validation = Spi::get_two::<i64, bool>(
        "SELECT count(*), bool_and(pg_catalog.amvalidate(opclass.oid))
           FROM pg_catalog.pg_opclass AS opclass
           JOIN pg_catalog.pg_am AS access_method
             ON access_method.oid = opclass.opcmethod
           JOIN pg_catalog.pg_namespace AS namespace
             ON namespace.oid = opclass.opcnamespace
          WHERE access_method.amname = 'pgcontext_hnsw'
            AND namespace.nspname = 'pgcontext'",
    )
    .expect("built-in HNSW opclass validation query failed");
    assert_eq!(builtin_validation, (Some(14), Some(true)));

    Spi::run(
        "CREATE TEMP TABLE pgcontext_variant_hnsw_items (
            id integer PRIMARY KEY,
            half_value pgcontext.halfvec NOT NULL,
            sparse_value pgcontext.sparsevec NOT NULL,
            bit_value pgcontext.bitvec NOT NULL
        )",
    )
    .expect("variant HNSW unsupported table creation failed");

    Spi::run(
        "INSERT INTO pgcontext_variant_hnsw_items (id, half_value, sparse_value, bit_value) VALUES
            (30, pgcontext.halfvec('[2,0]'), pgcontext.sparsevec('{1:2}/2'), pgcontext.bitvec('11')),
            (10, pgcontext.halfvec('[1,0]'), pgcontext.sparsevec('{1:1}/2'), pgcontext.bitvec('10')),
            (20, pgcontext.halfvec('[0,1]'), pgcontext.sparsevec('{2:1}/2'), pgcontext.bitvec('01'))",
    )
    .expect("variant HNSW fixture insert failed");

    Spi::run(
        "CREATE INDEX pgcontext_variant_hnsw_half_idx
            ON pgcontext_variant_hnsw_items USING pgcontext_hnsw (half_value)",
    )
    .expect("halfvec HNSW index creation failed");
    Spi::run(
        "CREATE INDEX pgcontext_variant_hnsw_sparse_idx
            ON pgcontext_variant_hnsw_items USING pgcontext_hnsw (sparse_value)",
    )
    .expect("sparsevec HNSW index creation failed");
    Spi::run(
        "CREATE INDEX pgcontext_variant_hnsw_bit_hamming_idx
            ON pgcontext_variant_hnsw_items
         USING pgcontext_hnsw (bit_value pgcontext.bitvec_hnsw_hamming_ops)",
    )
    .expect("bitvec Hamming HNSW index creation failed");

    let variant_hnsw_opclasses = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT opcname::text,
                        opcintype = 'pgcontext.halfvec'::regtype,
                        opcintype = 'pgcontext.sparsevec'::regtype,
                        opcintype = 'pgcontext.bitvec'::regtype,
                        opckeytype = 'pgcontext.vector'::regtype
                   FROM pg_catalog.pg_opclass opclass
                   JOIN pg_catalog.pg_am am
                     ON am.oid = opclass.opcmethod
                  WHERE am.amname = 'pgcontext_hnsw'
                    AND (
                        opcintype IN ('pgcontext.halfvec'::regtype, 'pgcontext.sparsevec'::regtype, 'pgcontext.bitvec'::regtype)
                        OR opcname IN ('halfvec_hnsw_ops', 'sparsevec_hnsw_ops', 'bitvec_hnsw_hamming_ops')
                    )
                  ORDER BY 1",
                None,
                &[],
            )
            .expect("variant HNSW opclass catalog query failed");

        let mut rows = Vec::new();
        for row in result {
            let opcname = row.get::<String>(1)?.unwrap_or_default();
            let is_halfvec = row.get::<bool>(2)?.unwrap_or_default();
            let is_sparsevec = row.get::<bool>(3)?.unwrap_or_default();
            let is_bitvec = row.get::<bool>(4)?.unwrap_or_default();
            let stores_vector = row.get::<bool>(5)?.unwrap_or_default();
            rows.push((opcname, is_halfvec, is_sparsevec, is_bitvec, stores_vector));
        }
        Ok::<_, spi::Error>(rows)
    })
    .expect("variant HNSW opclass catalog rows failed");

    assert_eq!(
        variant_hnsw_opclasses,
        vec![
            ("bitvec_hnsw_hamming_ops".to_owned(), false, false, true, true),
            ("bitvec_hnsw_jaccard_ops".to_owned(), false, false, true, true),
            ("halfvec_hnsw_cosine_ops".to_owned(), true, false, false, true),
            ("halfvec_hnsw_ip_ops".to_owned(), true, false, false, true),
            ("halfvec_hnsw_l1_ops".to_owned(), true, false, false, true),
            ("halfvec_hnsw_ops".to_owned(), true, false, false, true),
            ("sparsevec_hnsw_cosine_ops".to_owned(), false, true, false, true),
            ("sparsevec_hnsw_ip_ops".to_owned(), false, true, false, true),
            ("sparsevec_hnsw_l1_ops".to_owned(), false, true, false, true),
            ("sparsevec_hnsw_ops".to_owned(), false, true, false, true)
        ],
        "promoted dense-storage variant HNSW opclasses should be present"
    );

    Spi::run("SET LOCAL enable_seqscan = off")
        .expect("seqscan should be disabled for halfvec HNSW plan checks");

    let plan = Spi::connect(|client| {
        let result = client
            .select(
                "EXPLAIN (COSTS TRUE, FORMAT TEXT)
                 SELECT id
                   FROM pgcontext_variant_hnsw_items
                  ORDER BY half_value OPERATOR(pgcontext.<->) pgcontext.halfvec('[0,0]')
                  LIMIT 1",
                None,
                &[],
            )
            .expect("halfvec HNSW EXPLAIN query failed");

        let mut lines = Vec::new();
        for row in result {
            lines.push(row.get::<String>(1)?.unwrap_or_default());
        }
        Ok::<_, spi::Error>(lines.join("\n"))
    })
    .expect("halfvec HNSW EXPLAIN plan rows failed");

    assert!(
        plan.contains("Index Scan using pgcontext_variant_hnsw_half_idx"),
        "expected halfvec HNSW index scan for order-by query:\n{plan}"
    );

    let indexed_rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT id,
                        half_value OPERATOR(pgcontext.<->) pgcontext.halfvec('[0,0]') AS exact_score
                   FROM pgcontext_variant_hnsw_items
                  ORDER BY half_value OPERATOR(pgcontext.<->) pgcontext.halfvec('[0,0]'), id
                  LIMIT 3",
                None,
                &[],
            )
            .expect("halfvec HNSW indexed order query failed");

        let mut rows = Vec::new();
        for row in result {
            rows.push((row.get::<i32>(1)?.unwrap(), row.get::<f32>(2)?.unwrap()));
        }
        Ok::<_, spi::Error>(rows)
    })
    .expect("halfvec HNSW indexed order rows failed");

    assert_eq!(indexed_rows, vec![(10, 1.0), (20, 1.0), (30, 2.0)]);

    let halfvec_exact_rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT id,
                        half_value OPERATOR(pgcontext.<->) pgcontext.halfvec('[0,0]') AS exact_score
                   FROM (VALUES
                        (30, pgcontext.halfvec('[2,0]')),
                        (10, pgcontext.halfvec('[1,0]')),
                        (20, pgcontext.halfvec('[0,1]'))
                   ) AS fixture(id, half_value)
                  ORDER BY exact_score, id
                  LIMIT 3",
                None,
                &[],
            )
            .expect("halfvec exact oracle query failed");

        let mut rows = Vec::new();
        for row in result {
            rows.push((row.get::<i32>(1)?.unwrap(), row.get::<f32>(2)?.unwrap()));
        }
        Ok::<_, spi::Error>(rows)
    })
    .expect("halfvec exact oracle rows failed");

    assert_eq!(indexed_rows, halfvec_exact_rows);

    let sparse_plan = Spi::connect(|client| {
        let result = client
            .select(
                "EXPLAIN (COSTS TRUE, FORMAT TEXT)
                 SELECT id
                   FROM pgcontext_variant_hnsw_items
                  ORDER BY sparse_value OPERATOR(pgcontext.<->) pgcontext.sparsevec('{}/2')
                  LIMIT 1",
                None,
                &[],
            )
            .expect("sparsevec HNSW EXPLAIN query failed");

        let mut lines = Vec::new();
        for row in result {
            lines.push(row.get::<String>(1)?.unwrap_or_default());
        }
        Ok::<_, spi::Error>(lines.join("\n"))
    })
    .expect("sparsevec HNSW EXPLAIN plan rows failed");

    assert!(
        sparse_plan.contains("Index Scan using pgcontext_variant_hnsw_sparse_idx"),
        "expected sparsevec HNSW index scan for order-by query:\n{sparse_plan}"
    );

    let sparse_indexed_rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT id,
                        sparse_value OPERATOR(pgcontext.<->) pgcontext.sparsevec('{}/2') AS exact_score
                   FROM pgcontext_variant_hnsw_items
                  ORDER BY sparse_value OPERATOR(pgcontext.<->) pgcontext.sparsevec('{}/2'), id
                  LIMIT 3",
                None,
                &[],
            )
            .expect("sparsevec HNSW indexed order query failed");

        let mut rows = Vec::new();
        for row in result {
            rows.push((row.get::<i32>(1)?.unwrap(), row.get::<f32>(2)?.unwrap()));
        }
        Ok::<_, spi::Error>(rows)
    })
    .expect("sparsevec HNSW indexed order rows failed");

    assert_eq!(sparse_indexed_rows, vec![(10, 1.0), (20, 1.0), (30, 2.0)]);

    let sparse_exact_rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT point_id, score
                   FROM pgcontext.search_sparse(
                        pgcontext.sparsevec('{}/2'),
                        ARRAY[30, 10, 20]::bigint[],
                        ARRAY[
                            pgcontext.sparsevec('{1:2}/2'),
                            pgcontext.sparsevec('{1:1}/2'),
                            pgcontext.sparsevec('{2:1}/2')
                        ],
                        'l2',
                        3
                   )",
                None,
                &[],
            )
            .expect("sparse exact oracle query failed");

        let mut rows = Vec::new();
        for row in result {
            rows.push((row.get::<i64>(1)?.unwrap(), row.get::<f32>(2)?.unwrap()));
        }
        Ok::<_, spi::Error>(rows)
    })
    .expect("sparse exact oracle rows failed");

    assert_eq!(
        sparse_indexed_rows,
        sparse_exact_rows
            .into_iter()
            .map(|(point_id, score)| (point_id as i32, score))
            .collect::<Vec<_>>()
    );

    let bit_plan = Spi::connect(|client| {
        let result = client
            .select(
                "EXPLAIN (COSTS TRUE, FORMAT TEXT)
                 SELECT id
                   FROM pgcontext_variant_hnsw_items
                  ORDER BY bit_value OPERATOR(pgcontext.<~>) pgcontext.bitvec('00')
                  LIMIT 1",
                None,
                &[],
            )
            .expect("bitvec Hamming HNSW EXPLAIN query failed");

        let mut lines = Vec::new();
        for row in result {
            lines.push(row.get::<String>(1)?.unwrap_or_default());
        }
        Ok::<_, spi::Error>(lines.join("\n"))
    })
    .expect("bitvec Hamming HNSW EXPLAIN plan rows failed");

    assert!(
        bit_plan.contains("Index Scan using pgcontext_variant_hnsw_bit_hamming_idx"),
        "expected bitvec Hamming HNSW index scan for order-by query:\n{bit_plan}"
    );

    let bit_rows = Spi::connect(|client| {
        let result = client.select(
            "SELECT id, bit_value OPERATOR(pgcontext.<~>) pgcontext.bitvec('00') AS exact_score
               FROM pgcontext_variant_hnsw_items
              ORDER BY bit_value OPERATOR(pgcontext.<~>) pgcontext.bitvec('00'), id
              LIMIT 3",
            None,
            &[],
        )?;
        result
            .map(|row| Ok((row.get::<i32>(1)?.unwrap(), row.get::<i32>(2)?.unwrap())))
            .collect::<Result<Vec<_>, spi::Error>>()
    })
    .expect("bitvec Hamming HNSW serving rows failed");
    assert_eq!(bit_rows, vec![(10, 1), (20, 1), (30, 2)]);

    Spi::run(
        "UPDATE pgcontext_variant_hnsw_items
            SET half_value = pgcontext.halfvec('[0,0]'),
                sparse_value = pgcontext.sparsevec('{}/2'),
                bit_value = pgcontext.bitvec('00')
          WHERE id = 30",
    )
    .expect("variant HNSW update fixture failed");
    Spi::run("DELETE FROM pgcontext_variant_hnsw_items WHERE id = 20")
        .expect("variant HNSW delete fixture failed");
    Spi::run(
        "INSERT INTO pgcontext_variant_hnsw_items (id, half_value, sparse_value, bit_value)
         VALUES (40, pgcontext.halfvec('[0.5,0]'), pgcontext.sparsevec('{1:0.5}/2'), pgcontext.bitvec('10'))",
    )
    .expect("variant HNSW insert fixture failed");

    let maintained_rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT id,
                        half_value OPERATOR(pgcontext.<->) pgcontext.halfvec('[0,0]') AS exact_score
                   FROM pgcontext_variant_hnsw_items
                  ORDER BY half_value OPERATOR(pgcontext.<->) pgcontext.halfvec('[0,0]'), id
                  LIMIT 3",
                None,
                &[],
            )
            .expect("halfvec HNSW maintained order query failed");

        let mut rows = Vec::new();
        for row in result {
            rows.push((row.get::<i32>(1)?.unwrap(), row.get::<f32>(2)?.unwrap()));
        }
        Ok::<_, spi::Error>(rows)
    })
    .expect("halfvec HNSW maintained order rows failed");

    assert_eq!(maintained_rows, vec![(30, 0.0), (40, 0.5), (10, 1.0)]);

    let halfvec_maintained_exact_rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT id,
                        half_value OPERATOR(pgcontext.<->) pgcontext.halfvec('[0,0]') AS exact_score
                   FROM (VALUES
                        (30, pgcontext.halfvec('[0,0]')),
                        (40, pgcontext.halfvec('[0.5,0]')),
                        (10, pgcontext.halfvec('[1,0]'))
                   ) AS fixture(id, half_value)
                  ORDER BY exact_score, id
                  LIMIT 3",
                None,
                &[],
            )
            .expect("halfvec maintained exact oracle query failed");

        let mut rows = Vec::new();
        for row in result {
            rows.push((row.get::<i32>(1)?.unwrap(), row.get::<f32>(2)?.unwrap()));
        }
        Ok::<_, spi::Error>(rows)
    })
    .expect("halfvec maintained exact oracle rows failed");

    assert_eq!(maintained_rows, halfvec_maintained_exact_rows);

    let sparse_maintained_rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT id,
                        sparse_value OPERATOR(pgcontext.<->) pgcontext.sparsevec('{}/2') AS exact_score
                   FROM pgcontext_variant_hnsw_items
                  ORDER BY sparse_value OPERATOR(pgcontext.<->) pgcontext.sparsevec('{}/2'), id
                  LIMIT 3",
                None,
                &[],
            )
            .expect("sparsevec HNSW maintained order query failed");

        let mut rows = Vec::new();
        for row in result {
            rows.push((row.get::<i32>(1)?.unwrap(), row.get::<f32>(2)?.unwrap()));
        }
        Ok::<_, spi::Error>(rows)
    })
    .expect("sparsevec HNSW maintained order rows failed");

    assert_eq!(
        sparse_maintained_rows,
        vec![(30, 0.0), (40, 0.5), (10, 1.0)]
    );

    let sparse_maintained_exact_rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT point_id, score
                   FROM pgcontext.search_sparse(
                        pgcontext.sparsevec('{}/2'),
                        ARRAY[30, 40, 10]::bigint[],
                        ARRAY[
                            pgcontext.sparsevec('{}/2'),
                            pgcontext.sparsevec('{1:0.5}/2'),
                            pgcontext.sparsevec('{1:1}/2')
                        ],
                        'l2',
                        3
                   )",
                None,
                &[],
            )
            .expect("sparse maintained exact oracle query failed");

        let mut rows = Vec::new();
        for row in result {
            rows.push((row.get::<i64>(1)?.unwrap(), row.get::<f32>(2)?.unwrap()));
        }
        Ok::<_, spi::Error>(rows)
    })
    .expect("sparse maintained exact oracle rows failed");

    assert_eq!(
        sparse_maintained_rows,
        sparse_maintained_exact_rows
            .into_iter()
            .map(|(point_id, score)| (point_id as i32, score))
            .collect::<Vec<_>>()
    );

    let bit_maintained_rows = Spi::connect(|client| {
        let result = client.select(
            "SELECT id, bit_value OPERATOR(pgcontext.<~>) pgcontext.bitvec('00') AS exact_score
               FROM pgcontext_variant_hnsw_items
              ORDER BY bit_value OPERATOR(pgcontext.<~>) pgcontext.bitvec('00'), id
              LIMIT 3",
            None,
            &[],
        )?;
        result
            .map(|row| Ok((row.get::<i32>(1)?.unwrap(), row.get::<i32>(2)?.unwrap())))
            .collect::<Result<Vec<_>, spi::Error>>()
    })
    .expect("bitvec Hamming maintained HNSW rows failed");
    assert_eq!(bit_maintained_rows, vec![(30, 0), (10, 1), (40, 1)]);

    assert_vector_compat_ddl_failure(
        "INSERT INTO pgcontext_variant_hnsw_items (id, half_value, sparse_value, bit_value)
         VALUES (50, pgcontext.halfvec('[1]'), pgcontext.sparsevec('{1:1}/2'), pgcontext.bitvec('1'))",
        "22023",
        "failed to insert HNSW graph node: dimension mismatch: left has 2 dimensions, right has 1",
        "halfvec HNSW insert dimension mismatch",
    );
    assert_vector_compat_ddl_failure(
        "UPDATE pgcontext_variant_hnsw_items
            SET half_value = pgcontext.halfvec('[1]')
          WHERE id = 10",
        "22023",
        "failed to insert HNSW graph node: dimension mismatch: left has 2 dimensions, right has 1",
        "halfvec HNSW update dimension mismatch",
    );
    assert_vector_compat_ddl_failure(
        "INSERT INTO pgcontext_variant_hnsw_items (id, half_value, sparse_value, bit_value)
         VALUES (60, pgcontext.halfvec('[1,0]'), pgcontext.sparsevec('{1:1}/1'), pgcontext.bitvec('1'))",
        "22023",
        "failed to insert HNSW graph node: dimension mismatch: left has 2 dimensions, right has 1",
        "sparsevec HNSW insert dimension mismatch",
    );
    assert_vector_compat_ddl_failure(
        "UPDATE pgcontext_variant_hnsw_items
            SET sparse_value = pgcontext.sparsevec('{1:1}/1')
          WHERE id = 10",
        "22023",
        "failed to insert HNSW graph node: dimension mismatch: left has 2 dimensions, right has 1",
        "sparsevec HNSW update dimension mismatch",
    );
    assert_vector_compat_ddl_failure(
        "INSERT INTO pgcontext_variant_hnsw_items (id, half_value, sparse_value, bit_value)
         VALUES (70, pgcontext.halfvec('[1,0]'), pgcontext.sparsevec('{1:1}/2'), pgcontext.bitvec('1'))",
        "22023",
        "failed to insert HNSW graph node: dimension mismatch: left has 2 dimensions, right has 1",
        "bitvec Hamming HNSW insert dimension mismatch",
    );
    assert_vector_compat_ddl_failure(
        "UPDATE pgcontext_variant_hnsw_items
            SET bit_value = pgcontext.bitvec('1')
          WHERE id = 10",
        "22023",
        "failed to insert HNSW graph node: dimension mismatch: left has 2 dimensions, right has 1",
        "bitvec Hamming HNSW update dimension mismatch",
    );
    assert_vector_compat_ddl_failure(
        "SELECT id
           FROM pgcontext_variant_hnsw_items
          ORDER BY bit_value OPERATOR(pgcontext.<~>) pgcontext.bitvec('1')
          LIMIT 1",
        "22023",
        "failed to search persisted HNSW pages: dimension mismatch: left has 2 dimensions, right has 1",
        "bitvec Hamming HNSW query dimension mismatch",
    );

    Spi::run(
        "CREATE TEMP TABLE pgcontext_variant_hnsw_bad_half_items (
            half_value pgcontext.halfvec NOT NULL
        )",
    )
    .expect("bad halfvec HNSW fixture table creation failed");
    Spi::run(
        "INSERT INTO pgcontext_variant_hnsw_bad_half_items VALUES
            (pgcontext.halfvec('[1,0]')),
            (pgcontext.halfvec('[1]'))",
    )
    .expect("bad halfvec HNSW fixture insert failed");
    assert_vector_compat_ddl_failure(
        "CREATE INDEX pgcontext_variant_hnsw_bad_half_idx
            ON pgcontext_variant_hnsw_bad_half_items USING pgcontext_hnsw (half_value)",
        "22023",
        "failed to build HNSW graph: dimension mismatch: left has 2 dimensions, right has 1",
        "halfvec HNSW dimension mismatch",
    );

    Spi::run(
        "CREATE TEMP TABLE pgcontext_variant_hnsw_bad_sparse_items (
            sparse_value pgcontext.sparsevec NOT NULL
        )",
    )
    .expect("bad sparsevec HNSW fixture table creation failed");
    Spi::run(
        "INSERT INTO pgcontext_variant_hnsw_bad_sparse_items VALUES
            (pgcontext.sparsevec('{1:1}/2')),
            (pgcontext.sparsevec('{1:1}/1'))",
    )
    .expect("bad sparsevec HNSW fixture insert failed");
    assert_vector_compat_ddl_failure(
        "CREATE INDEX pgcontext_variant_hnsw_bad_sparse_idx
            ON pgcontext_variant_hnsw_bad_sparse_items USING pgcontext_hnsw (sparse_value)",
        "22023",
        "failed to build HNSW graph: dimension mismatch: left has 2 dimensions, right has 1",
        "sparsevec HNSW dimension mismatch",
    );

    Spi::run(
        "CREATE TEMP TABLE pgcontext_variant_hnsw_bad_bit_items (
            bit_value pgcontext.bitvec NOT NULL
        )",
    )
    .expect("bad bitvec Hamming HNSW fixture table creation failed");
    Spi::run(
        "INSERT INTO pgcontext_variant_hnsw_bad_bit_items VALUES
            (pgcontext.bitvec('10')),
            (pgcontext.bitvec('1'))",
    )
    .expect("bad bitvec Hamming HNSW fixture insert failed");
    assert_vector_compat_ddl_failure(
        "CREATE INDEX pgcontext_variant_hnsw_bad_bit_idx
            ON pgcontext_variant_hnsw_bad_bit_items
         USING pgcontext_hnsw (bit_value pgcontext.bitvec_hnsw_hamming_ops)",
        "22023",
        "failed to build HNSW graph: dimension mismatch: left has 2 dimensions, right has 1",
        "bitvec Hamming HNSW dimension mismatch",
    );
    Spi::run(
        "CREATE OPERATOR CLASS pgcontext_variant_halfvec_hnsw_bad_cosine_operator_ops
            FOR TYPE pgcontext.halfvec USING pgcontext_hnsw AS
            OPERATOR 1 pgcontext.<=> (pgcontext.halfvec, pgcontext.halfvec) FOR ORDER BY pg_catalog.float_ops,
            FUNCTION 1 pgcontext.halfvec_l2_distance(pgcontext.halfvec, pgcontext.halfvec),
            STORAGE pgcontext.vector",
    )
    .expect("bad halfvec cosine-operator HNSW opclass fixture creation failed");
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT pg_catalog.amvalidate(opclass.oid)
               FROM pg_catalog.pg_opclass AS opclass
              WHERE opclass.opcname = 'pgcontext_variant_halfvec_hnsw_bad_cosine_operator_ops'",
        )
        .expect("bad custom HNSW opclass validation query failed"),
        Some(false),
    );
    assert_vector_compat_ddl_failure(
        "CREATE INDEX pgcontext_variant_hnsw_bad_half_cosine_operator_idx
            ON pgcontext_variant_hnsw_items
         USING pgcontext_hnsw (half_value pgcontext_variant_halfvec_hnsw_bad_cosine_operator_ops)",
        "42P17",
        "HNSW opclass must use certified pgcontext.<->",
        "halfvec HNSW unsupported strategy operator",
    );
    Spi::run(
        "CREATE OPERATOR CLASS pgcontext_variant_sparsevec_hnsw_bad_cosine_operator_ops
            FOR TYPE pgcontext.sparsevec USING pgcontext_hnsw AS
            OPERATOR 1 pgcontext.<=> (pgcontext.sparsevec, pgcontext.sparsevec) FOR ORDER BY pg_catalog.float_ops,
            FUNCTION 1 pgcontext.sparsevec_l2_distance(pgcontext.sparsevec, pgcontext.sparsevec),
            STORAGE pgcontext.vector",
    )
    .expect("bad sparsevec cosine-operator HNSW opclass fixture creation failed");
    assert_vector_compat_ddl_failure(
        "CREATE INDEX pgcontext_variant_hnsw_bad_sparse_cosine_operator_idx
            ON pgcontext_variant_hnsw_items
         USING pgcontext_hnsw (sparse_value pgcontext_variant_sparsevec_hnsw_bad_cosine_operator_ops)",
        "42P17",
        "HNSW opclass must use certified pgcontext.<->",
        "sparsevec HNSW unsupported strategy operator",
    );
    Spi::run(
        "CREATE OPERATOR CLASS pgcontext_variant_bitvec_hnsw_bad_jaccard_ops
            FOR TYPE pgcontext.bitvec USING pgcontext_hnsw AS
            OPERATOR 1 pgcontext.<%> (pgcontext.bitvec, pgcontext.bitvec) FOR ORDER BY pg_catalog.float_ops,
            FUNCTION 1 pgcontext.bitvec_jaccard_distance(pgcontext.bitvec, pgcontext.bitvec),
            STORAGE pgcontext.vector",
    )
    .expect("bitvec Jaccard HNSW opclass fixture creation failed");
    assert_eq!(
        Spi::get_one::<bool>(
            "SELECT pg_catalog.amvalidate(opclass.oid)
               FROM pg_catalog.pg_opclass AS opclass
              WHERE opclass.opcname = 'pgcontext_variant_bitvec_hnsw_bad_jaccard_ops'",
        )
        .expect("valid custom HNSW opclass validation query failed"),
        Some(true),
    );
    Spi::run(
        "CREATE INDEX pgcontext_variant_hnsw_jaccard_idx
            ON pgcontext_variant_hnsw_items
         USING pgcontext_hnsw (bit_value pgcontext_variant_bitvec_hnsw_bad_jaccard_ops)",
    )
    .expect("bitvec Jaccard HNSW index creation failed");
    let jaccard_rows = Spi::connect(|client| {
        let result = client.select(
            "SELECT id, bit_value OPERATOR(pgcontext.<%>) pgcontext.bitvec('10') AS exact_score
               FROM pgcontext_variant_hnsw_items
              ORDER BY bit_value OPERATOR(pgcontext.<%>) pgcontext.bitvec('10'), id
              LIMIT 3",
            None,
            &[],
        )?;
        result
            .map(|row| Ok((row.get::<i32>(1)?.unwrap(), row.get::<f64>(2)?.unwrap())))
            .collect::<Result<Vec<_>, spi::Error>>()
    })
    .expect("bitvec Jaccard HNSW serving rows failed");
    assert_eq!(jaccard_rows, vec![(10, 0.0), (40, 0.0), (30, 1.0)]);
    Spi::run(
        "CREATE OPERATOR CLASS pgcontext_variant_bitvec_hnsw_bad_jaccard_operator_ops
            FOR TYPE pgcontext.bitvec USING pgcontext_hnsw AS
            OPERATOR 1 pgcontext.<%> (pgcontext.bitvec, pgcontext.bitvec) FOR ORDER BY pg_catalog.float_ops,
            FUNCTION 1 pgcontext.bitvec_hamming_distance(pgcontext.bitvec, pgcontext.bitvec),
            STORAGE pgcontext.vector",
    )
    .expect("bad bitvec Jaccard-operator HNSW opclass fixture creation failed");
    assert_vector_compat_ddl_failure(
        "CREATE INDEX pgcontext_variant_hnsw_bad_jaccard_operator_idx
            ON pgcontext_variant_hnsw_items
         USING pgcontext_hnsw (bit_value pgcontext_variant_bitvec_hnsw_bad_jaccard_operator_ops)",
        "42P17",
        "HNSW opclass must use certified pgcontext.<~>",
        "bitvec HNSW unsupported strategy operator",
    );

    let cases = [
        (
            "CREATE INDEX pgcontext_variant_hnsw_bit_idx
                ON pgcontext_variant_hnsw_items USING pgcontext_hnsw (bit_value)",
            "42704",
            "data type bitvec has no default operator class for access method \"pgcontext_hnsw\"",
            "bitvec HNSW index",
        ),
    ];

    for (sql, sqlstate, message, context) in cases {
        assert_vector_compat_ddl_failure(sql, sqlstate, message, context);
    }
}

#[pg_test]
fn non_dense_hnsw_opclasses_match_exact_oracles_with_bounded_candidates() {
    Spi::run(
        "SET LOCAL pgcontext.hnsw_m = 16;
         SET LOCAL pgcontext.hnsw_ef_construction = 256;
         SET LOCAL pgcontext.hnsw_ef_search = 240;
         SET LOCAL pgcontext.hnsw_shared_serving = off;
         CREATE TEMP TABLE pgcontext_non_dense_hnsw_oracle (
             id integer PRIMARY KEY,
             half_value pgcontext.halfvec NOT NULL,
             sparse_value pgcontext.sparsevec NOT NULL,
             bit_value pgcontext.bitvec NOT NULL
         );
         INSERT INTO pgcontext_non_dense_hnsw_oracle
         SELECT id,
                format('[%s,%s,%s,%s]', id % 17 + 1, id % 19 + 1, id % 23 + 1, id % 29 + 1)::halfvec,
                format('{1:%s,2:%s,3:%s,4:%s}/4', id % 17 + 1, id % 19 + 1, id % 23 + 1, id % 29 + 1)::sparsevec,
                (id::bit(16))::bitvec
           FROM generate_series(1, 256) AS id",
    )
    .expect("non-dense HNSW oracle fixture should build");

    let cases = [
        ("half_l2", "half_value", "halfvec_hnsw_ops", "<->", "pgcontext.halfvec('[3,5,7,11]')"),
        ("half_ip", "half_value", "halfvec_hnsw_ip_ops", "<#>", "pgcontext.halfvec('[3,5,7,11]')"),
        ("half_cosine", "half_value", "halfvec_hnsw_cosine_ops", "<=>", "pgcontext.halfvec('[3,5,7,11]')"),
        ("half_l1", "half_value", "halfvec_hnsw_l1_ops", "<+>", "pgcontext.halfvec('[3,5,7,11]')"),
        ("sparse_l2", "sparse_value", "sparsevec_hnsw_ops", "<->", "pgcontext.sparsevec('{1:3,2:5,3:7,4:11}/4')"),
        ("sparse_ip", "sparse_value", "sparsevec_hnsw_ip_ops", "<#>", "pgcontext.sparsevec('{1:3,2:5,3:7,4:11}/4')"),
        ("sparse_cosine", "sparse_value", "sparsevec_hnsw_cosine_ops", "<=>", "pgcontext.sparsevec('{1:3,2:5,3:7,4:11}/4')"),
        ("sparse_l1", "sparse_value", "sparsevec_hnsw_l1_ops", "<+>", "pgcontext.sparsevec('{1:3,2:5,3:7,4:11}/4')"),
        ("bit_hamming", "bit_value", "bitvec_hnsw_hamming_ops", "<~>", "pgcontext.bitvec('0000000010101010')"),
        ("bit_jaccard", "bit_value", "bitvec_hnsw_jaccard_ops", "<%>", "pgcontext.bitvec('0000000010101010')"),
    ];

    for (suffix, column, opclass, operator, query) in cases {
        let index_name = format!("pgcontext_non_dense_hnsw_{suffix}_idx");
        Spi::run(&format!(
            "CREATE INDEX {index_name}
                ON pgcontext_non_dense_hnsw_oracle
             USING pgcontext_hnsw ({column} pgcontext.{opclass})"
        ))
        .expect("non-dense HNSW opclass should build");

        Spi::run(
            "SET LOCAL enable_indexscan = off;
             SET LOCAL enable_bitmapscan = off;
             SET LOCAL enable_seqscan = on",
        )
        .expect("exact oracle should use a sequential scan");
        let exact = Spi::get_one::<Vec<i32>>(&format!(
            "SELECT array_agg(id)
               FROM (
                    SELECT id
                      FROM pgcontext_non_dense_hnsw_oracle
                     ORDER BY {column} OPERATOR(pgcontext.{operator}) {query}, id
                     LIMIT 10
               ) oracle"
        ))
        .expect("non-dense exact oracle should execute")
        .unwrap_or_default();

        Spi::run(
            "SET LOCAL enable_indexscan = on;
             SET LOCAL enable_bitmapscan = off;
             SET LOCAL enable_seqscan = off",
        )
        .expect("non-dense ANN query should use an index scan");
        let plan = Spi::connect(|client| {
            let result = client.select(
                &format!(
                    "EXPLAIN (COSTS FALSE, FORMAT TEXT)
                     SELECT id
                       FROM pgcontext_non_dense_hnsw_oracle
                      ORDER BY {column} OPERATOR(pgcontext.{operator}) {query}, id
                      LIMIT 10"
                ),
                None,
                &[],
            )?;
            result
                .map(|row| Ok(row.get::<String>(1)?.unwrap_or_default()))
                .collect::<Result<Vec<_>, spi::Error>>()
                .map(|lines| lines.join("\n"))
        })
        .expect("non-dense HNSW plan should be readable");
        assert!(
            plan.contains(&format!("Index Scan using {index_name}")),
            "{suffix} query did not use its HNSW index:\n{plan}"
        );
        let indexed = Spi::get_one::<Vec<i32>>(&format!(
            "SELECT array_agg(id)
               FROM (
                    SELECT id
                      FROM pgcontext_non_dense_hnsw_oracle
                     ORDER BY {column} OPERATOR(pgcontext.{operator}) {query}, id
                     LIMIT 10
               ) approximate"
        ))
        .expect("non-dense HNSW query should execute")
        .unwrap_or_default();
        assert_eq!(indexed, exact, "{suffix} HNSW order diverged from its exact oracle");

        let candidate_count = Spi::get_one::<i64>(
            "SELECT candidates FROM pgcontext.hnsw_last_scan_work()",
        )
        .expect("non-dense HNSW work counters should be readable")
        .unwrap_or_default();
        assert!(candidate_count > 0, "{suffix} HNSW scan produced no candidates");
        assert!(
            candidate_count < 256,
            "{suffix} HNSW scan scored the full collection: {candidate_count}"
        );
    }
}

#[pg_test]
fn bitvec_jaccard_hnsw_rechecks_overlapping_float4_bounds() {
    Spi::run(
        "SET LOCAL pgcontext.hnsw_shared_serving = off;
         CREATE TEMP TABLE pgcontext_bit_jaccard_recheck (
             id integer PRIMARY KEY,
             bit_value pgcontext.bitvec NOT NULL
         );
         INSERT INTO pgcontext_bit_jaccard_recheck VALUES
             (1, pgcontext.bitvec(repeat('1', 1998))),
             (2, pgcontext.bitvec(repeat('1', 1996) || '00'));
         CREATE INDEX pgcontext_bit_jaccard_recheck_idx
             ON pgcontext_bit_jaccard_recheck
          USING pgcontext_hnsw (bit_value pgcontext.bitvec_hnsw_jaccard_ops);
         SET LOCAL enable_indexscan = on;
         SET LOCAL enable_bitmapscan = off;
         SET LOCAL enable_seqscan = off",
    )
    .expect("bitvec Jaccard reorder fixture should build");

    let query = "pgcontext.bitvec(repeat('1', 1997) || '0')";
    let plan = Spi::connect(|client| {
        let result = client.select(
            &format!(
                "EXPLAIN (COSTS FALSE, FORMAT TEXT)
                 SELECT id
                   FROM pgcontext_bit_jaccard_recheck
                  ORDER BY bit_value OPERATOR(pgcontext.<%>) {query}
                  LIMIT 1"
            ),
            None,
            &[],
        )?;
        result
            .map(|row| Ok(row.get::<String>(1)?.unwrap_or_default()))
            .collect::<Result<Vec<_>, spi::Error>>()
            .map(|lines| lines.join("\n"))
    })
    .expect("bitvec Jaccard reorder plan should be readable");
    assert!(
        plan.contains("Index Scan using pgcontext_bit_jaccard_recheck_idx"),
        "bitvec Jaccard reorder query did not use its HNSW index:\n{plan}"
    );

    let nearest = Spi::get_one::<i32>(&format!(
        "SELECT id
           FROM pgcontext_bit_jaccard_recheck
          ORDER BY bit_value OPERATOR(pgcontext.<%>) {query}
          LIMIT 1"
    ))
    .expect("bitvec Jaccard reorder query should execute");
    assert_eq!(nearest, Some(1));

    let rechecks = Spi::get_one::<i64>("SELECT rechecks FROM pgcontext.hnsw_last_scan_work()")
        .expect("bitvec Jaccard reorder work should be readable")
        .unwrap_or_default();
    assert!(
        rechecks >= 2,
        "bitvec Jaccard exact reorder did not consume both overlapping bounds: {rechecks}"
    );
}

#[pg_test]
fn non_dense_hnsw_rejects_records_above_the_single_page_envelope() {
    Spi::run(
        "CREATE TEMP TABLE pgcontext_non_dense_hnsw_oversized (
             bit_value pgcontext.bitvec NOT NULL
         );
         INSERT INTO pgcontext_non_dense_hnsw_oversized
         VALUES (pgcontext.bitvec(repeat('1', 8001)))",
    )
    .expect("oversized non-dense HNSW fixture should build");

    assert_vector_compat_ddl_failure(
        "CREATE INDEX pgcontext_non_dense_hnsw_oversized_idx
            ON pgcontext_non_dense_hnsw_oversized
         USING pgcontext_hnsw (bit_value pgcontext.bitvec_hnsw_jaccard_ops)",
        "54000",
        "HNSW vector record exceeds single-page storage limit: 32032 bytes (maximum 8064); reduce vector dimensions or hnsw_m",
        "non-dense HNSW single-page record envelope",
    );
}

#[pg_test]
fn non_dense_hnsw_indexes_ignore_null_source_values() {
    Spi::run(
        "CREATE TEMP TABLE pgcontext_non_dense_hnsw_nullable (
             id integer PRIMARY KEY,
             half_value pgcontext.halfvec,
             sparse_value pgcontext.sparsevec,
             bit_value pgcontext.bitvec
         );
         INSERT INTO pgcontext_non_dense_hnsw_nullable VALUES
             (1, '[1,0]'::halfvec, '{1:1}/2'::sparsevec, '10'::bitvec),
             (2, NULL, NULL, NULL);
         CREATE INDEX pgcontext_non_dense_hnsw_nullable_half_idx
             ON pgcontext_non_dense_hnsw_nullable
          USING pgcontext_hnsw (half_value pgcontext.halfvec_hnsw_ops);
         CREATE INDEX pgcontext_non_dense_hnsw_nullable_sparse_idx
             ON pgcontext_non_dense_hnsw_nullable
          USING pgcontext_hnsw (sparse_value pgcontext.sparsevec_hnsw_ops);
         CREATE INDEX pgcontext_non_dense_hnsw_nullable_bit_idx
             ON pgcontext_non_dense_hnsw_nullable
          USING pgcontext_hnsw (bit_value pgcontext.bitvec_hnsw_hamming_ops);
         SET LOCAL enable_seqscan = off;
         SET LOCAL enable_bitmapscan = off",
    )
    .expect("non-dense HNSW indexes should ignore NULL values while building");

    for (column, operator, query, expected_index) in [
        (
            "half_value",
            "<->",
            "'[1,0]'::halfvec",
            "pgcontext_non_dense_hnsw_nullable_half_idx",
        ),
        (
            "sparse_value",
            "<->",
            "'{1:1}/2'::sparsevec",
            "pgcontext_non_dense_hnsw_nullable_sparse_idx",
        ),
        (
            "bit_value",
            "<~>",
            "'10'::bitvec",
            "pgcontext_non_dense_hnsw_nullable_bit_idx",
        ),
    ] {
        let plan = Spi::connect(|client| {
            let result = client.select(
                &format!(
                    "EXPLAIN (COSTS FALSE, FORMAT TEXT)
                     SELECT id
                       FROM pgcontext_non_dense_hnsw_nullable
                      ORDER BY {column} OPERATOR(pgcontext.{operator}) {query}
                      LIMIT 1"
                ),
                None,
                &[],
            )?;
            result
                .map(|row| Ok(row.get::<String>(1)?.unwrap_or_default()))
                .collect::<Result<Vec<_>, spi::Error>>()
                .map(|lines| lines.join("\n"))
        })
        .expect("nullable non-dense HNSW plan should execute");
        assert!(
            plan.contains(expected_index),
            "nullable non-dense HNSW query did not use {expected_index}: {plan}"
        );

        assert_eq!(
            Spi::get_one::<i32>(&format!(
                "SELECT id
                   FROM pgcontext_non_dense_hnsw_nullable
                  ORDER BY {column} OPERATOR(pgcontext.{operator}) {query}
                  LIMIT 1"
            ))
            .expect("nullable non-dense HNSW query should execute"),
            Some(1)
        );
    }
}

fn assert_vector_compat_sql_failure(sql: &str, sqlstate: &str, message: &str, context: &str) {
    let message = message.replace('\'', "''");
    Spi::run(&format!(
        r#"
        DO $$
        DECLARE
            actual_sqlstate text;
        BEGIN
            BEGIN
                PERFORM * FROM ({sql}) AS invalid_call;
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
    .expect("invalid vector compatibility call should raise expected error");
}

fn assert_vector_compat_ddl_failure(sql: &str, sqlstate: &str, message: &str, context: &str) {
    let escaped_sql = sql.replace('\'', "''");
    let message = message.replace('\'', "''");
    Spi::run(&format!(
        r#"
        DO $$
        DECLARE
            actual_sqlstate text;
        BEGIN
            BEGIN
                EXECUTE '{escaped_sql}';
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
    .expect("invalid vector compatibility DDL should raise expected error");
}
