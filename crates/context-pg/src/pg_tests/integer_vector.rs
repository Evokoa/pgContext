#[pg_test]
fn integer_vectors_round_trip_cast_and_score_exactly() {
    let row = Spi::connect(|client| {
        let row = client
            .select(
                "SELECT
                    pgcontext.int8vec('[-128,0,127]')::text,
                    pgcontext.uint8vec('[0,128,255]')::text,
                    pgcontext.int8vec_dims(pgcontext.int8vec('[1,2,3]')),
                    pgcontext.uint8vec_l2_distance(
                        pgcontext.uint8vec('[0,0]'), pgcontext.uint8vec('[3,4]')
                    ),
                    pgcontext.int8vec_inner_product(
                        pgcontext.int8vec('[-2,3]'), pgcontext.int8vec('[4,5]')
                    ),
                    pgcontext.uint8vec_inner_product(
                        pgcontext.uint8vec('[2,3]'), pgcontext.uint8vec('[4,5]')
                    ),
                    pgcontext.int8vec('[1,2,3]')::smallint[]",
                None,
                &[],
            )?
            .first();
        Ok::<_, spi::Error>((
            row.get::<String>(1)?.unwrap_or_default(),
            row.get::<String>(2)?.unwrap_or_default(),
            row.get::<i32>(3)?.unwrap_or_default(),
            row.get::<f64>(4)?.unwrap_or_default(),
            row.get::<f64>(5)?.unwrap_or_default(),
            row.get::<f64>(6)?.unwrap_or_default(),
            row.get::<Vec<i16>>(7)?.unwrap_or_default(),
        ))
    })
    .expect("integer vector row should decode");
    assert_eq!(row.0, "[-128,0,127]");
    assert_eq!(row.1, "[0,128,255]");
    assert_eq!(row.2, 3);
    assert_eq!(row.3, 5.0);
    assert_eq!(row.4, 7.0);
    assert_eq!(row.5, 23.0);
    assert_eq!(row.6, vec![1, 2, 3]);

    shared_assert_sql_failure(
        "SELECT ARRAY[128]::integer[]::pgcontext.int8vec",
        "22003",
        "int8vec coordinate is outside -128..127: 128",
        "signed integer vector overflow",
    );
    shared_assert_sql_failure(
        "SELECT pgcontext.vector('[1.5,2]')::pgcontext.uint8vec",
        "22003",
        "integer vector cast requires integral coordinates: 1.5",
        "fractional dense integer vector cast",
    );
}

#[pg_test]
fn integer_vector_aggregates_keep_wide_exact_transition_state() {
    let row = Spi::connect(|client| {
        let row = client
            .select(
                "SELECT
                    pgcontext.sum(value),
                    pgcontext.avg(value)
                   FROM (VALUES
                       (pgcontext.int8vec('[1,2]')),
                       (pgcontext.int8vec('[3,4]'))
                   ) AS values(value)",
                None,
                &[],
            )?
            .first();
        Ok::<_, spi::Error>((
            row.get::<Vec<i64>>(1)?.unwrap_or_default(),
            row.get::<Vec<f64>>(2)?.unwrap_or_default(),
        ))
    })
    .expect("integer vector aggregates should execute");
    assert_eq!(row.0, vec![4, 6]);
    assert_eq!(row.1, vec![2.0, 3.0]);
}

#[pg_test]
fn provider_binary_import_enforces_order_length_and_padding() {
    Spi::run(
        "CREATE TABLE public.provider_binary_probe (id bigint PRIMARY KEY, embedding bitvec(4));
         CREATE INDEX provider_binary_probe_hnsw
             ON public.provider_binary_probe USING pgcontext_hnsw
             (embedding pgcontext.bitvec_hnsw_hamming_ops);
         SELECT pgcontext.create_collection('provider_binary_probe', 'public.provider_binary_probe');
         SELECT pgcontext.register_embedding_profile(
             'provider_binary_probe', 'binary_v1', 'embedding',
             'public.provider_binary_probe_hnsw',
             '{
                \"representation\":\"bit\", \"dimensions\":4,
                \"normalization\":\"none\", \"metric\":\"hamming\",
                \"provider\":\"fixture\", \"model\":\"fixture\", \"revision\":\"v1\",
                \"input_template\":\"{text}\", \"output_template\":\"bits\",
                \"bit_order\":\"msb_first\", \"byte_order\":\"msb_first\",
                \"scale\":null, \"zero_point\":null,
                \"configuration_hash\":\"0000000000000001\"
              }'::jsonb
         )",
    )
    .expect("provider profile fixture should register");
    let bits = Spi::get_one::<String>(
        "SELECT pgcontext.bitvec_from_provider_bytes(
             'provider_binary_probe', 'binary_v1', decode('a0', 'hex')
         )::text",
    )
    .expect("provider byte import should run")
    .expect("provider byte import should return a value");
    assert_eq!(bits, "1010");
    shared_assert_sql_failure(
        "SELECT pgcontext.bitvec_from_provider_bytes(
             'provider_binary_probe', 'binary_v1', decode('a1', 'hex')
         )",
        "22P02",
        "invalid vector: provider binary padding bit 7 must be zero",
        "nonzero provider binary padding",
    );
}

#[pg_test]
fn integer_hnsw_opclasses_use_exact_source_recheck() {
    Spi::run(
        "CREATE TABLE integer_hnsw_probe (
             id bigint PRIMARY KEY,
             signed_embedding int8vec(3) NOT NULL,
             unsigned_embedding uint8vec(3) NOT NULL
         )",
    )
    .expect("integer HNSW table should be created");
    Spi::run(
        "INSERT INTO integer_hnsw_probe VALUES
             (1, pgcontext.int8vec('[1,0,0]'), pgcontext.uint8vec('[1,0,0]')),
             (2, pgcontext.int8vec('[3,4,0]'), pgcontext.uint8vec('[3,4,0]')),
             (3, pgcontext.int8vec('[-8,1,2]'), pgcontext.uint8vec('[8,1,2]'))",
    )
    .expect("integer HNSW rows should insert");
    Spi::run(
        "CREATE INDEX integer_hnsw_probe_signed
             ON integer_hnsw_probe USING pgcontext_hnsw
             (signed_embedding pgcontext.int8vec_hnsw_ops);
         CREATE INDEX integer_hnsw_probe_unsigned
             ON integer_hnsw_probe USING pgcontext_hnsw
             (unsigned_embedding pgcontext.uint8vec_hnsw_ops)",
    )
    .expect("integer HNSW indexes should build");
    let valid_opclasses = Spi::get_one::<i64>(
        "SELECT count(*)
           FROM pg_catalog.pg_opclass AS opclass
           JOIN pg_catalog.pg_namespace AS namespace
             ON namespace.oid = opclass.opcnamespace
          WHERE namespace.nspname = 'pgcontext'
            AND opclass.opcname IN (
                'int8vec_hnsw_ops', 'int8vec_hnsw_ip_ops',
                'int8vec_hnsw_cosine_ops', 'int8vec_hnsw_l1_ops',
                'uint8vec_hnsw_ops', 'uint8vec_hnsw_ip_ops',
                'uint8vec_hnsw_cosine_ops', 'uint8vec_hnsw_l1_ops'
            )
            AND pg_catalog.amvalidate(opclass.oid)",
    )
    .expect("integer opclass validation should execute");
    assert_eq!(valid_opclasses, Some(8));
    Spi::run("SET enable_seqscan = off").expect("index plan should be forced");
    let signed = Spi::get_one::<i64>(
        "SELECT id FROM integer_hnsw_probe
          ORDER BY signed_embedding OPERATOR(pgcontext.<->)
                   pgcontext.int8vec('[3,4,0]') LIMIT 1",
    )
    .expect("signed HNSW query should run");
    let unsigned = Spi::get_one::<i64>(
        "SELECT id FROM integer_hnsw_probe
          ORDER BY unsigned_embedding OPERATOR(pgcontext.<->)
                   pgcontext.uint8vec('[8,1,2]') LIMIT 1",
    )
    .expect("unsigned HNSW query should run");
    assert_eq!(signed, Some(2));
    assert_eq!(unsigned, Some(3));
    Spi::run("RESET enable_seqscan").expect("planner setting should reset");
}
