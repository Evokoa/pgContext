fn artifact_segment_rows(sql: &str) -> Vec<(String, i64, i64)> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push((
                row.get::<String>(1)?.expect("kind should not be null"),
                row.get::<i64>(2)?.expect("payload_bytes should not be null"),
                row.get::<i64>(3)?.expect("checksum should not be null"),
            ));
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("artifact segment SQL query should succeed")
}

fn hnsw_graph_artifact_rows(sql: &str) -> Vec<(i64, i32, i64)> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push((
                row.get::<i64>(1)?
                    .expect("record_count should not be null"),
                row.get::<i32>(2)?
                    .expect("dimensions should not be null"),
                row.get::<i64>(3)?
                    .expect("base_neighbor_count should not be null"),
            ));
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("HNSW graph artifact SQL query should succeed")
}

fn artifact_manifest_rows(sql: &str) -> Vec<ArtifactManifestRow> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push(ArtifactManifestRow {
                artifact_id: row.get::<i64>(1)?.expect("artifact_id should not be null"),
                collection_name: row
                    .get::<String>(2)?
                    .expect("collection_name should not be null"),
                build_job_id: row
                    .get::<i64>(3)?
                    .expect("build_job_id should not be null"),
                artifact_kind: row
                    .get::<String>(4)?
                    .expect("artifact_kind should not be null"),
                artifact_name: row
                    .get::<String>(5)?
                    .expect("artifact_name should not be null"),
                target_name: row
                    .get::<String>(6)?
                    .expect("target_name should not be null"),
                segment_kind: row
                    .get::<String>(7)?
                    .expect("segment_kind should not be null"),
                format_version: row
                    .get::<i32>(8)?
                    .expect("format_version should not be null"),
                payload_bytes: row
                    .get::<i64>(9)?
                    .expect("payload_bytes should not be null"),
                checksum: row.get::<i64>(10)?.expect("checksum should not be null"),
                lifecycle_state: row
                    .get::<String>(11)?
                    .expect("lifecycle_state should not be null"),
            });
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("artifact manifest SQL query should succeed")
}

fn artifact_file_rows(sql: &str) -> Vec<ArtifactFileRow> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push(ArtifactFileRow {
                artifact_id: row.get::<i64>(1)?.expect("artifact_id should not be null"),
                collection_name: row
                    .get::<String>(2)?
                    .expect("collection_name should not be null"),
                build_job_id: row
                    .get::<i64>(3)?
                    .expect("build_job_id should not be null"),
                artifact_kind: row
                    .get::<String>(4)?
                    .expect("artifact_kind should not be null"),
                artifact_name: row
                    .get::<String>(5)?
                    .expect("artifact_name should not be null"),
                target_name: row
                    .get::<String>(6)?
                    .expect("target_name should not be null"),
                segment_kind: row
                    .get::<String>(7)?
                    .expect("segment_kind should not be null"),
                format_version: row
                    .get::<i32>(8)?
                    .expect("format_version should not be null"),
                payload_bytes: row
                    .get::<i64>(9)?
                    .expect("payload_bytes should not be null"),
                checksum: row.get::<i64>(10)?.expect("checksum should not be null"),
                relative_path: row.get::<String>(11)?,
                lifecycle_state: row
                    .get::<String>(12)?
                    .expect("lifecycle_state should not be null"),
            });
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("artifact file SQL query should succeed")
}

fn artifact_memory_rows(sql: &str) -> Vec<ArtifactMemoryRow> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push(ArtifactMemoryRow {
                artifact_kind: row
                    .get::<String>(1)?
                    .expect("artifact_kind should not be null"),
                artifact_name: row
                    .get::<String>(2)?
                    .expect("artifact_name should not be null"),
                target_name: row
                    .get::<String>(3)?
                    .expect("target_name should not be null"),
                lifecycle_state: row
                    .get::<String>(4)?
                    .expect("lifecycle_state should not be null"),
                payload_bytes: row
                    .get::<i64>(5)?
                    .expect("payload_bytes should not be null"),
                header_bytes: row
                    .get::<i64>(6)?
                    .expect("header_bytes should not be null"),
                mapped_bytes: row
                    .get::<i64>(7)?
                    .expect("mapped_bytes should not be null"),
                file_materialized: row
                    .get::<bool>(8)?
                    .expect("file_materialized should not be null"),
            });
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("artifact memory SQL query should succeed")
}

fn completed_artifact_build_job(
    collection_name: &str,
    artifact_kind: &str,
    artifact_name: &str,
) -> i64 {
    create_artifact_collection(collection_name);
    let build_job_id = start_artifact_build_job(collection_name, artifact_kind, artifact_name, 0);
    Spi::run(&format!("SELECT pgcontext.run_build_job({build_job_id}, 1)"))
        .expect("artifact build job should complete");
    build_job_id
}

fn start_artifact_build_job(
    collection_name: &str,
    artifact_kind: &str,
    artifact_name: &str,
    total_units: i64,
) -> i64 {
    Spi::get_one::<i64>(&format!(
        "SELECT build_job_id
           FROM pgcontext.start_build_job(
                '{collection_name}',
                '{artifact_kind}',
                '{artifact_name}',
                'public.{collection_name}',
                {total_units}
           )"
    ))
    .expect("artifact build job should start")
    .expect("artifact build job should return an id")
}

fn create_artifact_collection(collection_name: &str) {
    Spi::run(&format!(
        "CREATE TABLE IF NOT EXISTS public.{collection_name} (
             id bigint PRIMARY KEY,
             embedding vector NOT NULL
         )"
    ))
    .expect("artifact source table should be created");
    Spi::run(&format!(
        "INSERT INTO public.{collection_name} (id, embedding)
         VALUES (10, '[1,0]'::vector)
         ON CONFLICT (id) DO NOTHING"
    ))
    .expect("artifact source row should be inserted");
    Spi::run(&format!(
        "DO $$
         BEGIN
             IF NOT EXISTS (
                 SELECT 1
                   FROM pgcontext._collections
                  WHERE collection_name = '{collection_name}'
             ) THEN
                 PERFORM pgcontext.create_collection(
                     '{collection_name}',
                     'public.{collection_name}'
                 );
             END IF;
         END $$"
    ))
    .expect("artifact collection should exist");
}

fn count_artifact_manifests(collection_name: &str) -> i64 {
    Spi::get_one::<i64>(&format!(
        "SELECT pg_catalog.count(*)::bigint
           FROM pgcontext.artifact_segments('{collection_name}')"
    ))
    .expect("artifact manifest count should succeed")
    .expect("artifact manifest count should not be null")
}

fn assert_artifact_file_publish_status_failure(
    build_job_id: i64,
    status: &str,
    context: &str,
) {
    shared_assert_sql_failure(
        &format!(
            "SELECT * FROM pgcontext.publish_artifact_segment_file(
                {build_job_id},
                pgcontext.encode_artifact_segment('hnsw_graph', decode('00', 'hex'))
            )"
        ),
        "55000",
        &format!("cannot publish artifact for build job {build_job_id} in status {status}"),
        context,
    );
    assert!(
        !artifact_file_exists(&artifact_file_path_for_build_job(build_job_id)),
        "{context} must not materialize a generated file"
    );
}

fn artifact_file_exists(relative_path: &str) -> bool {
    Spi::get_one_with_args::<bool>(
        "SELECT (pg_catalog.pg_stat_file($1, true)).size IS NOT NULL",
        &[relative_path.into()],
    )
    .expect("artifact file existence check should succeed")
    .expect("artifact file existence result should not be null")
}

fn artifact_file_path_for_build_job(build_job_id: i64) -> String {
    Spi::get_one_with_args::<String>(
        "SELECT 'pgcontext_artifacts/'
                || collection_id::text
                || '/'
                || build_job_id::text
                || '_'
                || artifact_kind
                || '.pgctxseg'
           FROM pgcontext._build_jobs
          WHERE build_job_id = $1",
        &[build_job_id.into()],
    )
    .expect("artifact file path lookup should succeed")
    .expect("artifact file path lookup should return a path")
}

fn caught_error_message(error: &pg_sys::panic::CaughtError) -> &str {
    match error {
        pg_sys::panic::CaughtError::PostgresError(report)
        | pg_sys::panic::CaughtError::ErrorReport(report) => report.message(),
        pg_sys::panic::CaughtError::RustPanic { ereport, .. } => ereport.message(),
    }
}
