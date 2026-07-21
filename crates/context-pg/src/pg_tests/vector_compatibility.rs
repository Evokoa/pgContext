#[pg_test]
fn pgvector_dense_input_output_and_dims_cases() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT '[1,2,3]'::vector::text AS compact,
                        '[1, 2, 3]'::vector::text AS spaced,
                        pgcontext.vector_dims('[1,2,3]'::vector) AS dims",
                None,
                &[],
            )
            .expect("dense vector input/output query failed");

        let row = result.first();
        Ok::<_, spi::Error>((
            row.get::<String>(1)?.unwrap_or_default(),
            row.get::<String>(2)?.unwrap_or_default(),
            row.get::<i32>(3)?.unwrap_or_default(),
        ))
    })
    .expect("dense vector input/output rows failed");

    assert_eq!(rows, ("[1,2,3]".to_owned(), "[1,2,3]".to_owned(), 3));
}

#[pg_test]
fn dense_vector_uses_the_pgvector_compatible_packed_f32_datum() {
    let bytes = Spi::get_one::<i32>("SELECT pg_column_size('[1,2,3]'::vector)")
        .expect("packed vector datum size should be readable")
        .expect("packed vector datum size should not be null");

    // Four-byte varlena header + pgvector's `{ int16 dim; int16 unused }`
    // header + three f32 values. Coexist mode requires byte-identical
    // datums so pgContext values bind to pgvector's type by name resolution;
    // the pre-coexist eight-byte versioned header this test once pinned was
    // retired by the P1 layout port.
    assert_eq!(bytes, 20);
}

#[pg_test]
fn pgvector_dense_distance_function_cases() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT pgcontext.l2_distance('[1,2,3]'::vector, '[4,6,3]'::vector),
                        pgcontext.inner_product('[1,2,3]'::vector, '[4,6,3]'::vector),
                        pgcontext.negative_inner_product('[1,2,3]'::vector, '[4,6,3]'::vector),
                        pgcontext.cosine_distance('[1,2,3]'::vector, '[4,6,3]'::vector),
                        pgcontext.l1_distance('[1,2,3]'::vector, '[4,6,3]'::vector)",
                None,
                &[],
            )
            .expect("dense vector distance query failed");

        let row = result.first();
        Ok::<_, spi::Error>((
            row.get::<f32>(1)?.unwrap_or_default(),
            row.get::<f32>(2)?.unwrap_or_default(),
            row.get::<f32>(3)?.unwrap_or_default(),
            row.get::<f32>(4)?.unwrap_or_default(),
            row.get::<f32>(5)?.unwrap_or_default(),
        ))
    })
    .expect("dense vector distance rows failed");

    assert_eq!(rows.0, 5.0);
    assert_eq!(rows.1, 25.0);
    assert_eq!(rows.2, -25.0);
    assert!((rows.3 - 0.144_517_61).abs() < 0.000_001);
    assert_eq!(rows.4, 7.0);
}

#[pg_test]
fn pgvector_dense_distance_functions_and_operators_are_strict() {
    let all_null = Spi::get_one::<bool>(
        "SELECT pgcontext.l2_distance(NULL::vector, '[1]'::vector) IS NULL
             AND pgcontext.inner_product('[1]'::vector, NULL::vector) IS NULL
             AND pgcontext.negative_inner_product(NULL::vector, '[1]'::vector) IS NULL
             AND pgcontext.cosine_distance('[1]'::vector, NULL::vector) IS NULL
             AND pgcontext.l1_distance(NULL::vector, '[1]'::vector) IS NULL
             AND (NULL::vector OPERATOR(pgcontext.<->) '[1]'::vector) IS NULL
             AND ('[1]'::vector OPERATOR(pgcontext.<#>) NULL::vector) IS NULL
             AND (NULL::vector OPERATOR(pgcontext.<=>) '[1]'::vector) IS NULL
             AND ('[1]'::vector OPERATOR(pgcontext.<+>) NULL::vector) IS NULL",
    )
    .expect("dense vector NULL semantics query failed")
    .unwrap_or(false);

    assert!(all_null);
}

#[pg_test]
#[should_panic(expected = "invalid vector: dense vectors must contain at least one value")]
fn pgvector_dense_input_rejects_empty_vectors() {
    Spi::run("SELECT '[]'::vector").expect("empty vector input should fail");
}

#[pg_test]
fn pgvector_real_array_cast_cases() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT ARRAY[1,2,3]::real[]::vector::text,
                        ARRAY[1,2,3]::integer[]::vector::text,
                        ARRAY[1,2,3]::double precision[]::vector::text,
                        '[1,2,3]'::vector::real[]",
                None,
                &[],
            )
            .expect("real array cast query failed");

        let row = result.first();
        Ok::<_, spi::Error>((
            row.get::<String>(1)?.unwrap_or_default(),
            row.get::<String>(2)?.unwrap_or_default(),
            row.get::<String>(3)?.unwrap_or_default(),
            row.get::<Vec<f32>>(4)?.unwrap_or_default(),
        ))
    })
    .expect("real array cast rows failed");

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
#[should_panic(expected = "cannot be represented exactly as a dense vector element")]
fn pgvector_integer_array_conversion_rejects_precision_loss() {
    Spi::run("SELECT pgcontext.vector_from_integer_array(ARRAY[16777217])")
        .expect("inexact integer conversion should fail");
}

#[pg_test]
#[should_panic(expected = "cannot be represented exactly as a dense vector element")]
fn pgvector_double_array_conversion_rejects_precision_loss() {
    Spi::run("SELECT pgcontext.vector_from_double_array(ARRAY[16777217.0]::double precision[])")
        .expect("inexact double conversion should fail");
}

#[pg_test]
fn narrowing_array_casts_are_explicit_only() {
    let assignment_casts = Spi::get_one::<i64>(
        "SELECT count(*)::bigint
           FROM pg_catalog.pg_cast
          WHERE castcontext <> 'e'
            AND (castsource, casttarget) IN (
                ('integer[]'::regtype, 'vector'::regtype),
                ('double precision[]'::regtype, 'vector'::regtype),
                ('real[]'::regtype, 'halfvec'::regtype),
                ('integer[]'::regtype, 'halfvec'::regtype),
                ('double precision[]'::regtype, 'halfvec'::regtype)
            )",
    )
    .expect("narrowing cast contexts should be inspectable")
    .expect("narrowing cast context count should not be null");

    assert_eq!(assignment_casts, 0);
}

#[pg_test]
#[should_panic(expected = "invalid vector: dense vectors must contain at least one value")]
fn pgvector_real_array_cast_rejects_empty_arrays() {
    Spi::run("SELECT ARRAY[]::real[]::vector").expect("empty real array cast should fail");
}

#[pg_test]
fn pgvector_dense_aggregate_cases() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT pgcontext.sum(value)::text,
                        pgcontext.avg(value)::text
                   FROM (VALUES ('[1,2]'::vector), ('[3,4]'::vector)) AS items(value)",
                None,
                &[],
            )
            .expect("dense vector aggregate query failed");

        let row = result.first();
        Ok::<_, spi::Error>((
            row.get::<String>(1)?.unwrap_or_default(),
            row.get::<String>(2)?.unwrap_or_default(),
        ))
    })
    .expect("dense vector aggregate rows failed");

    assert_eq!(rows, ("[4,6]".to_owned(), "[2,3]".to_owned()));
}

#[pg_test]
fn pgvector_dense_aggregates_return_null_for_empty_input() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT pgcontext.sum(value), pgcontext.avg(value)
                   FROM (SELECT NULL::vector AS value WHERE false) AS empty",
                None,
                &[],
            )
            .expect("empty aggregate query failed");

        let row = result.first();
        Ok::<_, spi::Error>((
            row.get::<crate::Vector>(1)?.is_none(),
            row.get::<crate::Vector>(2)?.is_none(),
        ))
    })
    .expect("empty aggregate rows failed");

    assert_eq!(rows, (true, true));
}

#[pg_test]
#[should_panic(expected = "dimension mismatch: left has 2 dimensions, right has 1")]
fn pgvector_dense_aggregates_reject_dimension_mismatch() {
    Spi::run(
        "SELECT pgcontext.sum(value)
           FROM (VALUES ('[1,2]'::vector), ('[3]'::vector)) AS items(value)",
    )
    .expect("dimension mismatch aggregate should fail");
}

#[pg_test]
fn pgvector_dense_distance_operator_cases() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT '[1,2,3]'::vector OPERATOR(pgcontext.<->) '[4,6,3]'::vector,
                        '[1,2,3]'::vector OPERATOR(pgcontext.<#>) '[4,6,3]'::vector,
                        '[1,2,3]'::vector OPERATOR(pgcontext.<=>) '[4,6,3]'::vector,
                        '[1,2,3]'::vector OPERATOR(pgcontext.<+>) '[4,6,3]'::vector",
                None,
                &[],
            )
            .expect("dense vector distance operator query failed");

        let row = result.first();
        Ok::<_, spi::Error>((
            row.get::<f64>(1)?.unwrap_or_default(),
            row.get::<f32>(2)?.unwrap_or_default(),
            row.get::<f32>(3)?.unwrap_or_default(),
            row.get::<f32>(4)?.unwrap_or_default(),
        ))
    })
    .expect("dense vector distance operator rows failed");

    assert_eq!(rows.0, 5.0);
    assert_eq!(rows.1, -25.0);
    assert!((rows.2 - 0.144_517_61).abs() < 0.000_001);
    assert_eq!(rows.3, 7.0);
}

#[pg_test]
fn pgvector_dense_distance_operators_support_order_by() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT id
                   FROM (VALUES
                        (30, '[2,0]'::vector),
                        (10, '[1,0]'::vector),
                        (20, '[0,1]'::vector)
                   ) AS items(id, embedding)
                  ORDER BY embedding OPERATOR(pgcontext.<->) '[0,0]'::vector, id",
                None,
                &[],
            )
            .expect("dense vector operator ordering query failed");

        let mut rows = Vec::new();
        for row in result {
            rows.push(row.get::<i32>(1)?.unwrap_or_default());
        }
        Ok::<_, spi::Error>(rows)
    })
    .expect("dense vector operator ordering rows failed");

    assert_eq!(rows, vec![10, 20, 30]);
}

#[pg_test]
fn pgvector_dense_comparison_operator_cases() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT '[1,2]'::vector OPERATOR(pgcontext.=) '[1,2]'::vector,
                        '[1,2]'::vector OPERATOR(pgcontext.<>) '[1,3]'::vector,
                        '[1,2]'::vector OPERATOR(pgcontext.<) '[1,3]'::vector,
                        '[1,2]'::vector OPERATOR(pgcontext.<=) '[1,2]'::vector,
                        '[1,3]'::vector OPERATOR(pgcontext.>) '[1,2]'::vector,
                        '[1,3]'::vector OPERATOR(pgcontext.>=) '[1,3]'::vector",
                None,
                &[],
            )
            .expect("dense vector comparison operator query failed");

        let row = result.first();
        Ok::<_, spi::Error>((
            row.get::<bool>(1)?.unwrap_or_default(),
            row.get::<bool>(2)?.unwrap_or_default(),
            row.get::<bool>(3)?.unwrap_or_default(),
            row.get::<bool>(4)?.unwrap_or_default(),
            row.get::<bool>(5)?.unwrap_or_default(),
            row.get::<bool>(6)?.unwrap_or_default(),
        ))
    })
    .expect("dense vector comparison operator rows failed");

    assert_eq!(rows, (true, true, true, true, true, true));
}

#[pg_test]
fn pgvector_dense_btree_ordering_cases() {
    Spi::run(
        "CREATE TEMP TABLE pgcontext_vector_btree_items (
            id integer PRIMARY KEY,
            value vector NOT NULL
        )",
    )
    .expect("dense vector btree table creation failed");
    Spi::run(
        "INSERT INTO pgcontext_vector_btree_items (id, value)
         VALUES
            (40, '[1,2]'::vector),
            (20, '[1,1]'::vector),
            (30, '[1,1,0]'::vector),
            (10, '[0,9]'::vector)",
    )
    .expect("dense vector btree rows insert failed");
    Spi::run(
        "CREATE INDEX pgcontext_vector_btree_items_value_idx
            ON pgcontext_vector_btree_items USING btree (value)",
    )
    .expect("dense vector btree index creation failed");

    let details = Spi::connect(|client| {
        let result = client
            .select(
                "SELECT
                    (
                        SELECT opcdefault
                          FROM pg_catalog.pg_opclass opclass
                          JOIN pg_catalog.pg_am am
                            ON am.oid = opclass.opcmethod
                         WHERE opcname = 'vector_ops'
                           AND am.amname = 'btree'
                    ) AS is_default,
                    pgcontext.vector_cmp('[1,1]'::vector, '[1,1,0]'::vector) AS cmp_prefix,
                    EXISTS (
                        SELECT 1
                          FROM pg_catalog.pg_indexes
                         WHERE indexname = 'pgcontext_vector_btree_items_value_idx'
                    ) AS has_index",
                None,
                &[],
            )
            .expect("dense vector btree metadata query failed");

        let row = result.first();
        Ok::<_, spi::Error>((
            row.get::<bool>(1)?.unwrap_or_default(),
            row.get::<i32>(2)?.unwrap_or_default(),
            row.get::<bool>(3)?.unwrap_or_default(),
        ))
    })
    .expect("dense vector btree metadata rows failed");

    assert_eq!(details, (true, -1, true));
}
