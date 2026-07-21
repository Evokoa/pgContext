#[pg_test]
fn quantization_sql_binary_scalar_and_product_good_paths() {
    let rows = Spi::connect(|client| {
        let result = client
            .select(
                r#"
                SELECT
                    pgcontext.binary_quantize('[1,-2,0,3.5]'::vector)::text,
                    pgcontext.scalar_quantize('[-1,-0.6,0.2,0.9]'::vector, -1.0::real, 1.0::real, 5),
                    pgcontext.scalar_reconstruct('\x000204'::bytea, -1.0::real, 1.0::real, 5)::text,
                    pgcontext.product_quantize(
                        '[0.9,0.1,-0.8,0.2]'::vector,
                        2,
                        '[[[0,0],[1,0]],[[-1,0],[0,1]]]'::jsonb
                    ),
                    pgcontext.product_reconstruct(
                        '\x0100'::bytea,
                        2,
                        '[[[0,0],[1,0]],[[-1,0],[0,1]]]'::jsonb
                    )::text
                "#,
                None,
                &[],
            )
            .expect("quantization SQL good-path query failed");

        let row = result.first();
        Ok::<_, spi::Error>((
            row.get::<String>(1)?.unwrap_or_default(),
            row.get::<Vec<u8>>(2)?.unwrap_or_default(),
            row.get::<String>(3)?.unwrap_or_default(),
            row.get::<Vec<u8>>(4)?.unwrap_or_default(),
            row.get::<String>(5)?.unwrap_or_default(),
        ))
    })
    .expect("quantization SQL good-path rows failed");

    assert_eq!(
        rows,
        (
            "1011".to_owned(),
            vec![0, 1, 2, 4],
            "[-1,0,1]".to_owned(),
            vec![1, 0],
            "[1,0,-1,0]".to_owned(),
        )
    );
}

#[pg_test]
fn quantization_sql_rejects_invalid_inputs() {
    let cases = [
        (
            "SELECT pgcontext.scalar_quantize('[1]'::vector, 1.0::real, 1.0::real, 5)",
            "invalid vector: invalid scalar quantization codebook: min must be less than max",
        ),
        (
            "SELECT pgcontext.scalar_reconstruct('\\x05'::bytea, -1.0::real, 1.0::real, 5)",
            "invalid vector: scalar quantized code 5 exceeds codebook levels 5",
        ),
        (
            "SELECT pgcontext.product_quantize('[1,0]'::vector, 2, '{}'::jsonb)",
            "product quantization codebooks must be a JSON array",
        ),
        (
            "SELECT pgcontext.product_quantize('[1,0]'::vector, 2, '[[[0,\"bad\"]]]'::jsonb)",
            "product quantization centroid 0.0.1 must be a number",
        ),
        (
            "SELECT pgcontext.product_quantize('[1,0]'::vector, 2, '[[[0,0]],[[1,0]]]'::jsonb)",
            "dimension mismatch: left has 4 dimensions, right has 2",
        ),
        (
            "SELECT pgcontext.product_reconstruct('\\x02'::bytea, 2, '[[[0,0],[1,0]]]'::jsonb)",
            "invalid vector: product quantized code 2 exceeds codebook size 2",
        ),
    ];

    for (sql, message) in cases {
        let message = message.replace('\'', "''");
        Spi::run(&format!(
            r#"
            DO $$
            BEGIN
                BEGIN
                    PERFORM * FROM ({sql}) AS invalid_call;
                    RAISE EXCEPTION 'expected quantization failure';
                EXCEPTION WHEN OTHERS THEN
                    IF SQLERRM <> '{message}' THEN
                        RAISE EXCEPTION 'unexpected quantization error: %', SQLERRM;
                    END IF;
                END;
            END $$;
            "#
        ))
        .expect("invalid quantization SQL should raise expected error");
    }
}
