//! SQL trigger helpers for automatic document chunking outbox jobs.

pgrx::extension_sql!(
    r#"
CREATE FUNCTION pgcontext._document_chunk_outbox_trigger()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pgcontext
AS $$
DECLARE
    source_row record;
    v_source_key text;
    old_source_key text;
    new_source_key text;
    v_source_version bigint;
    source_text text;
    source_text_bytes bigint;
    source_digest bytea;
    prior_id bigint;
    v_generation_id bigint;
BEGIN
    SELECT sources.*, aliases.chunking_profile_id AS current_profile_id,
           aliases.shadow_chunking_profile_id,
           rerank.source_table_oid, rerank.text_column_name,
           rerank.source_version_column_name, profiles.max_document_bytes
      INTO source_row
      FROM pgcontext._document_sources AS sources
      JOIN pgcontext._semantic_rerank_sources AS rerank USING (rerank_source_id)
      JOIN pgcontext._chunking_profile_aliases AS aliases
        USING (chunking_profile_alias_id)
      JOIN pgcontext._chunking_profiles AS profiles
        ON profiles.chunking_profile_id = aliases.chunking_profile_id
     WHERE sources.document_source_id = TG_ARGV[0]::bigint
       AND sources.status = 'ready' AND rerank.status = 'ready'
     FOR SHARE OF sources, aliases;
    IF NOT FOUND OR source_row.source_table_oid <> TG_RELID THEN
        RAISE EXCEPTION 'document chunk outbox source identity changed'
            USING ERRCODE = '55000';
    END IF;
    IF TG_OP <> 'INSERT' THEN old_source_key := OLD.id::text; END IF;
    IF TG_OP <> 'DELETE' THEN new_source_key := NEW.id::text; END IF;
    IF TG_OP = 'UPDATE' AND old_source_key IS DISTINCT FROM new_source_key THEN
        v_source_key := old_source_key;
        UPDATE pgcontext._document_chunk_generations SET status = 'retired'
         WHERE generation_id IN (
             SELECT generation_id FROM pgcontext._current_document_chunk_generations
              WHERE document_source_id = source_row.document_source_id
                AND source_key = v_source_key
                AND chunking_profile_id IN (
                    SELECT source_row.current_profile_id
                    UNION ALL
                    SELECT retained.chunking_profile_id
                      FROM pgcontext._chunking_profile_alias_retained AS retained
                     WHERE retained.chunking_profile_alias_id =
                           source_row.chunking_profile_alias_id
                )
         );
        UPDATE pgcontext._document_chunk_jobs AS jobs
           SET status = 'retired',
               lease_worker = NULL, lease_expires_at = NULL, updated_at = pg_catalog.now()
          FROM pgcontext._document_chunk_generations AS generations
         WHERE jobs.generation_id = generations.generation_id
           AND generations.document_source_id = source_row.document_source_id
           AND generations.source_key = v_source_key
           AND generations.status = 'retired';
        WITH target_jobs AS (
            SELECT jobs.job_id, jobs.generation_id
              FROM pgcontext._document_chunk_jobs AS jobs
              JOIN pgcontext._document_chunk_generations AS generations USING (generation_id)
             WHERE generations.document_source_id = source_row.document_source_id
               AND generations.source_key = v_source_key
               AND generations.chunking_profile_id IN (
                   source_row.current_profile_id,
                   COALESCE(source_row.shadow_chunking_profile_id,
                            source_row.current_profile_id)
               )
               AND jobs.status IN (
                   'queued','leased','parsing','chunking','embedding','validating',
                   'publishing','cancel_requested'
               )
             ORDER BY jobs.job_id LIMIT 256 FOR UPDATE OF jobs
        ), updated_jobs AS (
            UPDATE pgcontext._document_chunk_jobs AS jobs
               SET status = 'superseded', lease_worker = NULL,
                   lease_expires_at = NULL, updated_at = pg_catalog.now()
              FROM target_jobs
             WHERE jobs.job_id = target_jobs.job_id
            RETURNING jobs.job_id, jobs.generation_id
        ), updated_generations AS (
            UPDATE pgcontext._document_chunk_generations AS generations
               SET status = 'superseded'
              FROM updated_jobs
             WHERE generations.generation_id = updated_jobs.generation_id
            RETURNING updated_jobs.job_id
        )
        DELETE FROM pgcontext._document_chunk_staging AS staging
         USING updated_generations
         WHERE staging.job_id = updated_generations.job_id;
        DELETE FROM pgcontext._current_document_chunk_generations
         WHERE document_source_id = source_row.document_source_id
           AND source_key = v_source_key
           AND chunking_profile_id IN (
               SELECT source_row.current_profile_id
               UNION ALL
               SELECT retained.chunking_profile_id
                 FROM pgcontext._chunking_profile_alias_retained AS retained
                WHERE retained.chunking_profile_alias_id =
                      source_row.chunking_profile_alias_id
           );
    END IF;
    IF TG_OP = 'DELETE' THEN
        v_source_key := old_source_key;
        UPDATE pgcontext._document_chunk_generations AS generations
           SET status = 'retired'
         WHERE generations.generation_id IN (
            SELECT aliases.generation_id
              FROM pgcontext._current_document_chunk_generations AS aliases
             WHERE aliases.document_source_id = source_row.document_source_id
               AND aliases.source_key = v_source_key
               AND aliases.chunking_profile_id IN (
                   SELECT source_row.current_profile_id
                   UNION ALL
                   SELECT retained.chunking_profile_id
                     FROM pgcontext._chunking_profile_alias_retained AS retained
                    WHERE retained.chunking_profile_alias_id =
                          source_row.chunking_profile_alias_id
               )
         );
        UPDATE pgcontext._document_chunk_jobs AS jobs
           SET status = 'retired', lease_worker = NULL, lease_expires_at = NULL,
               updated_at = pg_catalog.now()
          FROM pgcontext._document_chunk_generations AS generations
         WHERE jobs.generation_id = generations.generation_id
           AND generations.document_source_id = source_row.document_source_id
           AND generations.source_key = v_source_key
           AND generations.status = 'retired';
        DELETE FROM pgcontext._current_document_chunk_generations
         WHERE document_source_id = source_row.document_source_id
           AND pgcontext._current_document_chunk_generations.source_key = v_source_key
           AND chunking_profile_id IN (
               SELECT source_row.current_profile_id
               UNION ALL
               SELECT retained.chunking_profile_id
                 FROM pgcontext._chunking_profile_alias_retained AS retained
                WHERE retained.chunking_profile_alias_id =
                      source_row.chunking_profile_alias_id
           );
        WITH target_jobs AS (
            SELECT jobs.job_id, jobs.generation_id
              FROM pgcontext._document_chunk_jobs AS jobs
              JOIN pgcontext._document_chunk_generations AS generations USING (generation_id)
             WHERE generations.document_source_id = source_row.document_source_id
               AND generations.source_key = v_source_key
               AND generations.chunking_profile_id IN (
                   source_row.current_profile_id,
                   COALESCE(source_row.shadow_chunking_profile_id,
                            source_row.current_profile_id)
               )
               AND jobs.status IN (
                   'queued','leased','parsing','chunking','embedding','validating',
                   'publishing','cancel_requested'
               )
             ORDER BY jobs.job_id LIMIT 256 FOR UPDATE OF jobs
        ), updated_jobs AS (
            UPDATE pgcontext._document_chunk_jobs AS jobs
               SET status = 'superseded', lease_worker = NULL,
                   lease_expires_at = NULL, updated_at = pg_catalog.now()
              FROM target_jobs
             WHERE jobs.job_id = target_jobs.job_id
            RETURNING jobs.job_id, jobs.generation_id
        ), updated_generations AS (
            UPDATE pgcontext._document_chunk_generations AS generations
               SET status = 'superseded'
              FROM updated_jobs
             WHERE generations.generation_id = updated_jobs.generation_id
            RETURNING updated_jobs.job_id
        )
        DELETE FROM pgcontext._document_chunk_staging AS staging
         USING updated_generations
         WHERE staging.job_id = updated_generations.job_id;
        RETURN OLD;
    END IF;
    v_source_key := new_source_key;
    EXECUTE pg_catalog.format(
        'SELECT pg_catalog.octet_length(($1).%1$I), (($1).%2$I)::bigint',
        source_row.text_column_name, source_row.source_version_column_name
    ) INTO source_text_bytes, v_source_version USING NEW;
    IF source_text_bytes IS NULL OR source_text_bytes > source_row.max_document_bytes THEN
        RAISE EXCEPTION 'document source text exceeds the registered profile byte limit'
            USING ERRCODE = '54000';
    END IF;
    EXECUTE pg_catalog.format('SELECT (($1).%I)::text', source_row.text_column_name)
       INTO source_text USING NEW;
    IF v_source_key IS NULL OR source_text IS NULL OR v_source_version IS NULL
       OR v_source_version <= 0 THEN
        RAISE EXCEPTION 'document chunk outbox source row is invalid'
            USING ERRCODE = '22023';
    END IF;
    source_digest := pg_catalog.sha256(pg_catalog.convert_to(source_text, 'UTF8'));
    SELECT aliases.generation_id INTO prior_id
      FROM pgcontext._current_document_chunk_generations AS aliases
     WHERE aliases.document_source_id = source_row.document_source_id
       AND aliases.source_key = v_source_key
       AND aliases.chunking_profile_id = source_row.current_profile_id;
    WITH target_jobs AS (
        SELECT jobs.job_id, jobs.generation_id
          FROM pgcontext._document_chunk_jobs AS jobs
          JOIN pgcontext._document_chunk_generations AS generations USING (generation_id)
         WHERE generations.document_source_id = source_row.document_source_id
           AND generations.source_key = v_source_key
           AND generations.chunking_profile_id = source_row.current_profile_id
           AND generations.source_version < v_source_version
           AND jobs.status IN (
               'queued','leased','parsing','chunking','embedding','validating',
               'publishing','cancel_requested'
           )
         ORDER BY jobs.job_id LIMIT 256 FOR UPDATE OF jobs
    ), updated_jobs AS (
        UPDATE pgcontext._document_chunk_jobs AS jobs
           SET status = 'superseded', lease_worker = NULL,
               lease_expires_at = NULL, updated_at = pg_catalog.now()
          FROM target_jobs
         WHERE jobs.job_id = target_jobs.job_id
        RETURNING jobs.job_id, jobs.generation_id
    ), updated_generations AS (
        UPDATE pgcontext._document_chunk_generations AS generations
           SET status = 'superseded'
          FROM updated_jobs
         WHERE generations.generation_id = updated_jobs.generation_id
        RETURNING updated_jobs.job_id
    )
    DELETE FROM pgcontext._document_chunk_staging AS staging
     USING updated_generations
     WHERE staging.job_id = updated_generations.job_id;
    INSERT INTO pgcontext._document_chunk_generations (
        document_source_id, source_key, source_version, source_sha256,
        chunking_profile_id, source_registration_revision, prior_generation_id
    ) VALUES (
        source_row.document_source_id, v_source_key, v_source_version, source_digest,
        source_row.current_profile_id, source_row.registration_revision, prior_id
    )
    ON CONFLICT (
        document_source_id, source_key, source_version, chunking_profile_id,
        source_registration_revision
    )
    DO UPDATE SET source_sha256 = pgcontext._document_chunk_generations.source_sha256
      WHERE pgcontext._document_chunk_generations.source_sha256 = EXCLUDED.source_sha256
        AND pgcontext._document_chunk_generations.source_registration_revision =
            EXCLUDED.source_registration_revision
    RETURNING generation_id INTO v_generation_id;
    IF v_generation_id IS NULL THEN
        RAISE EXCEPTION 'document source version identity changed'
            USING ERRCODE = '55000';
    END IF;
    INSERT INTO pgcontext._document_chunk_jobs (generation_id)
    VALUES (v_generation_id)
    ON CONFLICT (generation_id) DO NOTHING;
    RETURN NEW;
END;
$$;

CREATE FUNCTION pgcontext._install_document_chunk_outbox_trigger(p_document_source_id bigint)
RETURNS text
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pgcontext
AS $$
DECLARE
    source_row record;
    trigger_name text;
BEGIN
    PERFORM pgcontext._consume_document_chunk_permit(13, p_document_source_id, 0);
    SELECT sources.collection_id, rerank.source_schema_name, rerank.source_table_name,
           rerank.text_column_name, rerank.source_version_column_name
      INTO source_row
      FROM pgcontext._document_sources AS sources
      JOIN pgcontext._semantic_rerank_sources AS rerank USING (rerank_source_id)
     WHERE sources.document_source_id = p_document_source_id
       AND sources.status = 'ready' AND rerank.status = 'ready';
    IF NOT FOUND THEN
        RAISE EXCEPTION 'document source does not exist or is not ready'
            USING ERRCODE = '42704';
    END IF;
    PERFORM pgcontext._require_collection_owner(source_row.collection_id);
    trigger_name := pg_catalog.format('pgcontext_document_chunk_outbox_%s', p_document_source_id);
    EXECUTE pg_catalog.format(
        'DROP TRIGGER IF EXISTS %I ON %I.%I',
        trigger_name, source_row.source_schema_name, source_row.source_table_name
    );
    EXECUTE pg_catalog.format(
        'CREATE TRIGGER %I AFTER INSERT OR DELETE OR UPDATE OF id, %I, %I ON %I.%I
         FOR EACH ROW EXECUTE FUNCTION pgcontext._document_chunk_outbox_trigger(%L)',
        trigger_name, source_row.text_column_name, source_row.source_version_column_name,
        source_row.source_schema_name, source_row.source_table_name,
        p_document_source_id::text
    );
    RETURN trigger_name;
END;
$$;
"#,
    name = "create_document_chunking_outbox_functions",
    requires = ["create_document_chunking_profile_state"]
);
