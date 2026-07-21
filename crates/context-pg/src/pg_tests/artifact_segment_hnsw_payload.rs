#[pg_test]
fn artifact_segment_sql_validates_hnsw_graph_payload_metadata() {
    let rows = hnsw_graph_artifact_rows(
        "SELECT record_count, dimensions, base_neighbor_count
           FROM pgcontext.validate_hnsw_graph_artifact(
                pgcontext.encode_artifact_segment(
                    'hnsw_graph',
                    decode(
                        '5047435458484e53' ||
                        '01000000' ||
                        '02000000' ||
                        '02000000' ||
                        '00000000' ||
                        '00000000' || '01000000' || '6500000000000000' ||
                        '00000000' || '0000803f' ||
                        '01000000' ||
                        '01000000' || '01000000' || '6600000000000000' ||
                        '0000803f' || '00000000' ||
                        '00000000',
                        'hex'
                    )
                )
           )",
    );

    assert_eq!(rows, vec![(2, 2, 2)]);
}

#[pg_test]
fn artifact_segment_sql_rejects_corrupt_hnsw_graph_payload_with_sqlstate() {
    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.validate_hnsw_graph_artifact(
             pgcontext.encode_artifact_segment(
                 'hnsw_graph',
                 decode(
                     '5047435458484e53' ||
                     '01000000' ||
                     '01000000' ||
                     '01000000' ||
                     '00000000' ||
                     '00000000' || '01000000' || '6500000000000000' ||
                     '00000000' ||
                     '01000000',
                     'hex'
                 )
             )
         )",
        "XX001",
        "HNSW graph record 0 has out-of-range neighbor 1; record count is 1",
        "out-of-range HNSW graph neighbor",
    );
}

#[pg_test]
fn artifact_segment_sql_rejects_bad_hnsw_graph_payload_magic_with_sqlstate() {
    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.validate_hnsw_graph_artifact(
             pgcontext.encode_artifact_segment(
                 'hnsw_graph',
                 decode(
                     '5847435458484e53' ||
                     '01000000' ||
                     '01000000' ||
                     '01000000' ||
                     '00000000' ||
                     '00000000' || '00000000' || '6500000000000000' ||
                     '00000000',
                     'hex'
                 )
             )
         )",
        "XX001",
        "invalid HNSW graph payload magic",
        "bad HNSW graph payload magic",
    );
}

#[pg_test]
fn artifact_segment_sql_rejects_truncated_hnsw_graph_payload_with_sqlstate() {
    shared_assert_sql_failure(
        "SELECT * FROM pgcontext.validate_hnsw_graph_artifact(
             pgcontext.encode_artifact_segment(
                 'hnsw_graph',
                 decode('5047435458484e5301000000', 'hex')
             )
         )",
        "XX001",
        "truncated HNSW graph payload header: 12 < 24",
        "truncated HNSW graph payload",
    );
}
