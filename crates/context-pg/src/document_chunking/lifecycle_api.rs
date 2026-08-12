//! Less-common owner lifecycle operations for derived chunk generations.

use super::*;

/// Requeues a ready derived generation whose user-owned projection drifted.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn rebuild_document_chunk_job(job_id: i64) -> bool {
    require_authorized_job(job_id);
    require_buildable_job(job_id);
    let ((collection_id, source_name), generation_id) = Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT sources.collection_id, sources.source_name, generations.generation_id
                   FROM pgcontext._visible_document_chunk_jobs AS jobs
                   JOIN pgcontext._visible_document_chunk_generations AS generations
                     USING (generation_id)
                   JOIN pgcontext._visible_document_sources AS sources
                     ON sources.document_source_id = generations.document_source_id
                   JOIN pgcontext._visible_chunking_profile_aliases AS aliases
                     USING (chunking_profile_alias_id)
                  WHERE jobs.job_id = $1 AND jobs.status IN ('ready','retired')
                    AND generations.chunking_profile_id IN (
                        aliases.chunking_profile_id,
                        COALESCE(
                            aliases.shadow_chunking_profile_id,
                            aliases.chunking_profile_id
                        )
                    )",
                Some(1),
                &[job_id.into()],
            )
            .unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    "failed to load document chunk rebuild target",
                )
            });
        let row = rows.into_iter().next().unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                "document chunk generation is not rebuildable",
            )
        });
        (
            (
                required::<i64>(&row, 1, "collection identity"),
                required::<String>(&row, 2, "document source name"),
            ),
            required::<i64>(&row, 3, "generation identity"),
        )
    });
    require_collection_owner_id(collection_id);
    let source = load_source_registration(collection_id, &source_name);
    require_source_table_owner(&source);
    arm_document_chunk_permit(DocumentChunkPermitKind::Rebuild, job_id, 0);
    let rebuilt = Spi::get_one_with_args::<bool>(
        "SELECT pgcontext._rebuild_document_chunk_job($1)",
        &[job_id.into()],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "failed to requeue document chunk projection rebuild",
        )
    })
    .unwrap_or(false);
    if rebuilt {
        let projection = quote_qualified(&source.projection_schema, &source.projection_table);
        Spi::run_with_args(
            &format!("DELETE FROM {projection} WHERE generation_id = $1"),
            &[generation_id.into()],
        )
        .unwrap_or_else(|_| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
                "failed to clear document chunk projection for rebuild",
            )
        });
    }
    rebuilt
}
