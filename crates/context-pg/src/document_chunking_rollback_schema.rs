//! Atomic prior-generation rollback helper for automatic chunking.

pgrx::extension_sql!(
    r#"
CREATE FUNCTION pgcontext._rollback_document_chunk_generation(
    p_document_source_id bigint,
    p_source_key text,
    p_generation_id bigint
)
RETURNS bigint
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pgcontext
AS $$
DECLARE
    source_row record;
    target_row pgcontext._document_chunk_generations%ROWTYPE;
BEGIN
    PERFORM pgcontext._consume_document_chunk_permit(
        6, p_document_source_id, p_generation_id
    );
    SELECT sources.*, aliases.chunking_profile_id AS current_profile_id
      INTO source_row
      FROM pgcontext._document_sources AS sources
      JOIN pgcontext._chunking_profile_aliases AS aliases
        USING (chunking_profile_alias_id)
     WHERE sources.document_source_id = p_document_source_id
       AND sources.status = 'ready' AND aliases.status = 'ready'
     FOR SHARE OF sources, aliases;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'document source does not exist or is not ready'
            USING ERRCODE = '42704';
    END IF;
    PERFORM pgcontext._require_collection_owner(source_row.collection_id);
    PERFORM 1
      FROM pgcontext._document_chunk_jobs AS jobs
      JOIN pgcontext._document_chunk_generations AS generations USING (generation_id)
     WHERE generations.document_source_id = p_document_source_id
       AND generations.source_key = p_source_key
     ORDER BY jobs.job_id FOR UPDATE OF jobs;
    SELECT * INTO target_row FROM pgcontext._document_chunk_generations
     WHERE generation_id = p_generation_id
       AND document_source_id = p_document_source_id
       AND source_key = p_source_key
       AND chunking_profile_id = source_row.current_profile_id
       AND published_at IS NOT NULL
     FOR UPDATE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'document chunk rollback generation is unavailable'
            USING ERRCODE = '42704';
    END IF;
    UPDATE pgcontext._document_chunk_generations AS generations
       SET status = 'retired'
     WHERE generations.generation_id IN (
        SELECT aliases.generation_id
          FROM pgcontext._current_document_chunk_generations AS aliases
         WHERE aliases.document_source_id = p_document_source_id
           AND aliases.source_key = p_source_key
           AND aliases.chunking_profile_id = target_row.chunking_profile_id
     )
       AND generations.generation_id <> p_generation_id
       AND generations.status = 'ready';
    UPDATE pgcontext._document_chunk_jobs AS jobs
       SET status = 'retired', updated_at = pg_catalog.now()
      FROM pgcontext._document_chunk_generations AS generations
     WHERE jobs.generation_id = generations.generation_id
       AND generations.document_source_id = p_document_source_id
       AND generations.source_key = p_source_key
       AND generations.chunking_profile_id = target_row.chunking_profile_id
       AND generations.status = 'retired'
       AND generations.generation_id <> p_generation_id;
    UPDATE pgcontext._document_chunk_generations
       SET status = 'ready' WHERE generation_id = p_generation_id;
    UPDATE pgcontext._document_chunk_jobs
       SET status = 'ready', updated_at = pg_catalog.now()
     WHERE generation_id = p_generation_id;
    INSERT INTO pgcontext._current_document_chunk_generations (
        document_source_id, source_key, chunking_profile_id, generation_id
    ) VALUES (
        p_document_source_id, p_source_key, target_row.chunking_profile_id,
        p_generation_id
    )
    ON CONFLICT (document_source_id, source_key, chunking_profile_id) DO UPDATE
       SET generation_id = EXCLUDED.generation_id,
           publication_revision =
               pgcontext._current_document_chunk_generations.publication_revision + 1,
           updated_at = pg_catalog.clock_timestamp();
    RETURN p_generation_id;
END;
$$;
"#,
    name = "create_document_chunking_rollback_helper",
    requires = ["create_document_chunking_catalog_tables"]
);
