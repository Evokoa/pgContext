#[pg_test]
fn upsert_points_assigns_stable_point_ids() {
    create_point_collection("m2_points_stable");

    let inserted = point_upsert_rows(
        "SELECT point_id, source_key, inserted
           FROM pgcontext.upsert_points('m2_points_stable', ARRAY['doc-1', 'doc-2'])",
    );
    assert_eq!(inserted.len(), 2);
    assert_eq!(inserted[0].1, "doc-1");
    assert!(inserted[0].2);
    assert_eq!(inserted[1].1, "doc-2");
    assert!(inserted[1].2);

    let updated = point_upsert_rows(
        "SELECT point_id, source_key, inserted
           FROM pgcontext.upsert_points('m2_points_stable', ARRAY['doc-1'])",
    );
    assert_eq!(updated, vec![(inserted[0].0, "doc-1".to_owned(), false)]);
}

#[pg_test]
fn delete_points_marks_existing_points_and_ignores_missing_keys() {
    create_point_collection("m2_points_delete");
    let inserted = point_upsert_rows(
        "SELECT point_id, source_key, inserted
           FROM pgcontext.upsert_points('m2_points_delete', ARRAY['doc-1', 'doc-2'])",
    );

    let deleted = point_delete_rows(
        "SELECT point_id, source_key
           FROM pgcontext.delete_points('m2_points_delete', ARRAY['doc-2', 'missing'])",
    );

    assert_eq!(deleted, vec![(inserted[1].0, "doc-2".to_owned())]);
}

#[pg_test]
fn upsert_points_reactivates_deleted_points() {
    create_point_collection("m2_points_reactivate");
    let inserted = point_upsert_rows(
        "SELECT point_id, source_key, inserted
           FROM pgcontext.upsert_points('m2_points_reactivate', ARRAY['doc-1'])",
    );
    point_delete_rows(
        "SELECT point_id, source_key
           FROM pgcontext.delete_points('m2_points_reactivate', ARRAY['doc-1'])",
    );

    let reactivated = point_upsert_rows(
        "SELECT point_id, source_key, inserted
           FROM pgcontext.upsert_points('m2_points_reactivate', ARRAY['doc-1'])",
    );

    assert_eq!(
        reactivated,
        vec![(inserted[0].0, "doc-1".to_owned(), false)]
    );
}

#[pg_test]
fn bulk_upsert_points_reports_chunk_progress_and_existing_rows() {
    create_point_collection("m13_bulk_upsert");
    point_upsert_rows(
        "SELECT point_id, source_key, inserted
           FROM pgcontext.upsert_points('m13_bulk_upsert', ARRAY['doc-2'])",
    );

    let rows = bulk_upsert_rows(
        "SELECT batch_number, processed_count, inserted_count, reactivated_count
           FROM pgcontext.bulk_upsert_points(
                'm13_bulk_upsert',
                ARRAY['doc-1', 'doc-2', 'doc-3', 'doc-4', 'doc-5'],
                2
           )",
    );

    assert_eq!(rows, vec![(1, 2, 1, 1), (2, 2, 2, 0), (3, 1, 1, 0)]);
    assert_eq!(active_point_count("m13_bulk_upsert"), 5);
}

#[pg_test]
fn bulk_delete_points_reports_deleted_and_missing_rows_by_chunk() {
    create_point_collection("m13_bulk_delete");
    point_upsert_rows(
        "SELECT point_id, source_key, inserted
           FROM pgcontext.upsert_points(
                'm13_bulk_delete',
                ARRAY['doc-1', 'doc-2', 'doc-3']
           )",
    );

    let rows = bulk_delete_rows(
        "SELECT batch_number, processed_count, deleted_count, missing_count
           FROM pgcontext.bulk_delete_points(
                'm13_bulk_delete',
                ARRAY['doc-1', 'missing', 'doc-3'],
                2
           )",
    );

    assert_eq!(rows, vec![(1, 2, 1, 1), (2, 1, 1, 0)]);
    assert_eq!(active_point_count("m13_bulk_delete"), 1);
}

#[pg_test]
fn backfill_points_scans_source_table_in_bounded_batches() {
    create_point_collection("m13_backfill_points");
    Spi::run(
        "INSERT INTO public.m13_backfill_points (id, embedding)
         VALUES (1, '[1,0]'::vector),
                (2, '[2,0]'::vector),
                (3, '[3,0]'::vector),
                (4, '[4,0]'::vector),
                (5, '[5,0]'::vector)",
    )
    .expect("backfill source rows should be inserted");

    let rows = bulk_upsert_rows(
        "SELECT batch_number, processed_count, inserted_count, reactivated_count
           FROM pgcontext.backfill_points('m13_backfill_points', 2)",
    );

    assert_eq!(rows, vec![(1, 2, 2, 0), (2, 2, 2, 0), (3, 1, 1, 0)]);
    assert_eq!(active_point_count("m13_backfill_points"), 5);
}

#[pg_test]
#[should_panic(expected = "invalid point batch size: 0")]
fn bulk_upsert_points_rejects_zero_batch_size() {
    create_point_collection("m13_bulk_bad_batch");

    Spi::run(
        "SELECT pgcontext.bulk_upsert_points(
            'm13_bulk_bad_batch',
            ARRAY['doc-1'],
            0
        )",
    )
    .expect("zero batch size should be rejected");
}

#[pg_test]
#[should_panic(expected = "invalid source key: \"\"")]
fn bulk_upsert_points_rejects_invalid_keys_without_partial_insert() {
    create_point_collection("m13_bulk_bad_key");

    Spi::run(
        "SELECT pgcontext.bulk_upsert_points(
            'm13_bulk_bad_key',
            ARRAY['doc-1', '', 'doc-2'],
            2
        )",
    )
    .expect("invalid source key should be rejected");
}

#[pg_test]
#[should_panic(expected = "collection does not exist: m2_points_missing")]
fn upsert_points_rejects_missing_collections() {
    Spi::run("SELECT pgcontext.upsert_points('m2_points_missing', ARRAY['doc-1'])")
        .expect("missing collection should fail");
}

#[pg_test]
#[should_panic(expected = "collection has no source table: m2_points_no_table")]
fn upsert_points_rejects_collections_without_source_tables() {
    Spi::run("SELECT pgcontext.create_collection('m2_points_no_table')")
        .expect("collection should be created");

    Spi::run("SELECT pgcontext.upsert_points('m2_points_no_table', ARRAY['doc-1'])")
        .expect("collection without source table should fail");
}

#[pg_test]
#[should_panic(expected = "invalid source key: \"\"")]
fn upsert_points_rejects_invalid_source_keys() {
    create_point_collection("m2_points_bad_key");

    Spi::run("SELECT pgcontext.upsert_points('m2_points_bad_key', ARRAY[''])")
        .expect("invalid source key should fail");
}

fn create_point_collection(collection_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection_name} (
             id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
             embedding vector
         )"
    ))
    .expect("point source table should be created");
    Spi::run(&format!(
        "SELECT pgcontext.create_collection('{collection_name}', 'public.{collection_name}')"
    ))
    .expect("point collection should be created");
}

fn point_upsert_rows(sql: &str) -> Vec<(i64, String, bool)> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push((
                row.get::<i64>(1)?.expect("point_id should not be null"),
                row.get::<String>(2)?.expect("source_key should not be null"),
                row.get::<bool>(3)?.expect("inserted should not be null"),
            ));
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("point upsert rows query failed")
}

fn point_delete_rows(sql: &str) -> Vec<(i64, String)> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push((
                row.get::<i64>(1)?.expect("point_id should not be null"),
                row.get::<String>(2)?.expect("source_key should not be null"),
            ));
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("point delete rows query failed")
}

fn bulk_upsert_rows(sql: &str) -> Vec<(i64, i64, i64, i64)> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push((
                row.get::<i64>(1)?
                    .expect("batch_number should not be null"),
                row.get::<i64>(2)?
                    .expect("processed_count should not be null"),
                row.get::<i64>(3)?
                    .expect("inserted_count should not be null"),
                row.get::<i64>(4)?
                    .expect("reactivated_count should not be null"),
            ));
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("bulk upsert rows query failed")
}

fn bulk_delete_rows(sql: &str) -> Vec<(i64, i64, i64, i64)> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push((
                row.get::<i64>(1)?
                    .expect("batch_number should not be null"),
                row.get::<i64>(2)?
                    .expect("processed_count should not be null"),
                row.get::<i64>(3)?
                    .expect("deleted_count should not be null"),
                row.get::<i64>(4)?.expect("missing_count should not be null"),
            ));
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("bulk delete rows query failed")
}

fn active_point_count(collection_name: &str) -> i64 {
    Spi::get_one_with_args::<i64>(
        "SELECT count(*)::bigint
           FROM pgcontext._collection_points AS points
           JOIN pgcontext._collections AS collections USING (collection_id)
          WHERE collections.collection_name = $1
            AND points.deleted_at IS NULL",
        &[collection_name.into()],
    )
    .expect("active point count query should succeed")
    .expect("active point count should not be null")
}
