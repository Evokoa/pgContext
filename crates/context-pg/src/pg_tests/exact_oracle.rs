fn exact_oracle_rows(
    query: &str,
    metric: &str,
    limit: i32,
    candidates: &[(i64, &'static str)],
) -> Vec<(i64, f32)> {
    let point_ids = candidates
        .iter()
        .map(|(point_id, _)| point_id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let vectors = candidates
        .iter()
        .map(|(_, vector)| format!("'{vector}'::vector"))
        .collect::<Vec<_>>()
        .join(",");

    Spi::connect(|client| {
        let rows = client.select(
            &format!(
                "SELECT point_id, score
                   FROM pgcontext.search(
                        '{query}'::vector,
                        ARRAY[{point_ids}]::bigint[],
                        ARRAY[{vectors}]::vector[],
                        '{metric}',
                        {limit}
                   )"
            ),
            None,
            &[],
        )?;

        let mut output = Vec::new();
        for row in rows {
            output.push((
                row.get::<i64>(1)?
                    .expect("oracle point id should not be null"),
                row.get::<f32>(2)?.expect("oracle score should not be null"),
            ));
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("exact oracle query failed")
}

fn assert_source_key_scores_match_exact_oracle(
    table_rows: &[(i64, String, f32)],
    oracle_rows: &[(i64, f32)],
) {
    assert_eq!(
        table_rows.len(),
        oracle_rows.len(),
        "table search and exact oracle returned different row counts"
    );
    for ((_, table_source_key, table_score), (oracle_point_id, oracle_score)) in
        table_rows.iter().zip(oracle_rows.iter())
    {
        assert_eq!(table_source_key, &oracle_point_id.to_string());
        assert!(
            (table_score - oracle_score).abs() < 0.000_001,
            "score mismatch for source key {table_source_key}: table={table_score}, oracle={oracle_score}"
        );
    }
}

fn assert_source_keys_match_exact_oracle(
    source_keys: &[String],
    oracle_rows: &[(i64, f32)],
) {
    let oracle_source_keys = oracle_rows
        .iter()
        .map(|(point_id, _)| point_id.to_string())
        .collect::<Vec<_>>();
    assert_eq!(source_keys, oracle_source_keys.as_slice());
}
