//! Fenced staging, progress, release, and worker-failure lifecycle helpers.

pgrx::extension_sql!(
    r#"
CREATE FUNCTION pgcontext._load_document_chunk_staging(
    p_job_id bigint, p_lease_token bigint
)
RETURNS TABLE(
    response_json jsonb, response_sha256 bytea, chunk_count int4,
    token_count bigint, staging_bytes bigint
)
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pgcontext
AS $$
DECLARE collection_id bigint;
BEGIN
    PERFORM pgcontext._consume_document_chunk_permit(5, p_job_id, p_lease_token);
    SELECT sources.collection_id INTO collection_id
      FROM pgcontext._document_chunk_jobs AS jobs
      JOIN pgcontext._document_chunk_generations AS generations USING (generation_id)
      JOIN pgcontext._document_sources AS sources USING (document_source_id)
     WHERE jobs.job_id = p_job_id AND jobs.lease_token = p_lease_token;
    PERFORM pgcontext._require_collection_owner(collection_id);
    RETURN QUERY
    SELECT staging.response_json, staging.response_sha256, staging.chunk_count,
           staging.token_count, staging.staging_bytes
      FROM pgcontext._document_chunk_staging AS staging
     WHERE staging.job_id = p_job_id AND staging.lease_token = p_lease_token;
END;
$$;

CREATE FUNCTION pgcontext._invalidate_document_chunk_aliases(
    p_document_source_id bigint, p_source_keys text[]
)
RETURNS bigint
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pgcontext
AS $$
DECLARE collection_id bigint; deleted bigint;
BEGIN
    PERFORM pgcontext._consume_document_chunk_permit(
        17, p_document_source_id, pg_catalog.cardinality(p_source_keys)
    );
    SELECT sources.collection_id INTO collection_id FROM pgcontext._document_sources AS sources
     WHERE sources.document_source_id = p_document_source_id FOR SHARE;
    PERFORM pgcontext._require_collection_owner(collection_id);
    PERFORM 1
      FROM pgcontext._document_chunk_jobs AS jobs
      JOIN pgcontext._document_chunk_generations AS generations USING (generation_id)
     WHERE generations.document_source_id = p_document_source_id
       AND generations.source_key = ANY(p_source_keys)
     ORDER BY jobs.job_id FOR UPDATE OF jobs;
    UPDATE pgcontext._document_chunk_generations AS generations SET status = 'retired'
     WHERE generations.generation_id IN (
        SELECT aliases.generation_id FROM pgcontext._current_document_chunk_generations AS aliases
         WHERE aliases.document_source_id = p_document_source_id
           AND aliases.source_key = ANY(p_source_keys));
    GET DIAGNOSTICS deleted = ROW_COUNT;
    UPDATE pgcontext._document_chunk_jobs AS jobs
       SET status = 'retired', lease_worker = NULL, lease_expires_at = NULL,
           updated_at = pg_catalog.now()
      FROM pgcontext._document_chunk_generations AS generations
     WHERE jobs.generation_id = generations.generation_id
       AND generations.document_source_id = p_document_source_id
       AND generations.source_key = ANY(p_source_keys) AND generations.status = 'retired';
    UPDATE pgcontext._document_chunk_generations SET status = 'superseded'
     WHERE document_source_id = p_document_source_id AND source_key = ANY(p_source_keys)
       AND status NOT IN ('ready','retired','superseded');
    UPDATE pgcontext._document_chunk_jobs AS jobs
       SET status = 'superseded', lease_worker = NULL, lease_expires_at = NULL,
           updated_at = pg_catalog.now()
      FROM pgcontext._document_chunk_generations AS generations
     WHERE jobs.generation_id = generations.generation_id
       AND generations.document_source_id = p_document_source_id
       AND generations.source_key = ANY(p_source_keys) AND generations.status = 'superseded'
       AND jobs.status NOT IN ('ready','retired','superseded');
    DELETE FROM pgcontext._document_chunk_staging AS staging
     USING pgcontext._document_chunk_jobs AS jobs,
           pgcontext._document_chunk_generations AS generations
     WHERE staging.job_id = jobs.job_id AND jobs.generation_id = generations.generation_id
       AND generations.document_source_id = p_document_source_id
       AND generations.source_key = ANY(p_source_keys);
    DELETE FROM pgcontext._current_document_chunk_generations
     WHERE document_source_id = p_document_source_id AND source_key = ANY(p_source_keys);
    RETURN deleted;
END;
$$;

CREATE FUNCTION pgcontext._release_document_chunk_claim(
    p_job_id bigint, p_lease_token bigint
)
RETURNS boolean
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pgcontext
AS $$
DECLARE collection_id bigint;
BEGIN
    PERFORM pgcontext._consume_document_chunk_permit(8, p_job_id, p_lease_token);
    SELECT sources.collection_id INTO collection_id
      FROM pgcontext._document_chunk_jobs AS jobs
      JOIN pgcontext._document_chunk_generations AS generations USING (generation_id)
      JOIN pgcontext._document_sources AS sources USING (document_source_id)
     WHERE jobs.job_id = p_job_id AND jobs.lease_token = p_lease_token;
    PERFORM pgcontext._require_collection_owner(collection_id);
    UPDATE pgcontext._document_chunk_jobs
       SET status = 'queued', attempt = GREATEST(attempt - 1, 0),
           lease_worker = NULL, lease_expires_at = NULL,
           updated_at = pg_catalog.clock_timestamp()
     WHERE job_id = p_job_id AND lease_token = p_lease_token
       AND status IN ('leased','parsing','chunking','embedding','validating','publishing');
    IF FOUND THEN
        DELETE FROM pgcontext._document_chunk_staging WHERE job_id = p_job_id;
        UPDATE pgcontext._document_chunk_generations AS generations SET status = 'queued'
          FROM pgcontext._document_chunk_jobs AS jobs
         WHERE jobs.job_id = p_job_id AND generations.generation_id = jobs.generation_id;
        RETURN true;
    END IF;
    RETURN false;
END;
$$;

CREATE FUNCTION pgcontext._load_document_chunk_lease_expiry(
    p_job_id bigint, p_lease_token bigint
)
RETURNS bigint
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pgcontext
AS $$
DECLARE collection_id bigint; expires_micros bigint;
BEGIN
    PERFORM pgcontext._consume_document_chunk_permit(10, p_job_id, p_lease_token);
    SELECT sources.collection_id,
           COALESCE(
               pg_catalog.floor(pg_catalog.extract('epoch', jobs.lease_expires_at) * 1000000)::bigint,
               9223372036854775807::bigint
           )
      INTO collection_id, expires_micros
      FROM pgcontext._document_chunk_jobs AS jobs
      JOIN pgcontext._document_chunk_generations AS generations USING (generation_id)
      JOIN pgcontext._document_sources AS sources USING (document_source_id)
     WHERE jobs.job_id = p_job_id AND jobs.lease_token = p_lease_token
       AND (jobs.lease_expires_at > pg_catalog.clock_timestamp() OR jobs.status = 'ready');
    IF expires_micros IS NULL THEN
        RAISE EXCEPTION 'stale document chunk job lease' USING ERRCODE = '40001';
    END IF;
    PERFORM pgcontext._require_collection_owner(collection_id);
    RETURN expires_micros;
END;
$$;

CREATE FUNCTION pgcontext._load_document_chunk_claim_source(
    p_job_id bigint, p_lease_token bigint
)
RETURNS TABLE(document_source_id bigint, source_table_oid oid, source_key text)
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pgcontext
AS $$
DECLARE collection_id bigint;
BEGIN
    PERFORM pgcontext._consume_document_chunk_permit(24, p_job_id, p_lease_token);
    SELECT sources.collection_id
      INTO collection_id
      FROM pgcontext._document_chunk_jobs AS jobs
      JOIN pgcontext._document_chunk_generations AS generations USING (generation_id)
      JOIN pgcontext._document_sources AS sources USING (document_source_id)
      JOIN pgcontext._semantic_rerank_sources AS rerank USING (rerank_source_id)
     WHERE jobs.job_id = p_job_id AND jobs.lease_token = p_lease_token
       AND jobs.lease_expires_at > pg_catalog.clock_timestamp()
       AND jobs.status IN ('leased','parsing','chunking','embedding','validating','publishing');
    IF collection_id IS NULL THEN
        RAISE EXCEPTION 'stale document chunk job lease' USING ERRCODE = '40001';
    END IF;
    PERFORM pgcontext._require_collection_owner(collection_id);
    RETURN QUERY
    SELECT generations.document_source_id, rerank.source_table_oid,
           generations.source_key
      FROM pgcontext._document_chunk_jobs AS jobs
      JOIN pgcontext._document_chunk_generations AS generations USING (generation_id)
      JOIN pgcontext._document_sources AS sources USING (document_source_id)
      JOIN pgcontext._semantic_rerank_sources AS rerank USING (rerank_source_id)
     WHERE jobs.job_id = p_job_id AND jobs.lease_token = p_lease_token
       AND jobs.lease_expires_at > pg_catalog.clock_timestamp()
       AND jobs.status IN ('leased','parsing','chunking','embedding','validating','publishing');
END;
$$;

CREATE FUNCTION pgcontext._lock_document_chunk_source(
    p_document_source_id bigint, p_source_key text,
    p_source_version bigint, p_source_bytes bigint,
    p_max_document_bytes bigint
)
RETURNS bytea
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pgcontext
AS $$
DECLARE
    source_schema text;
    source_table text;
    source_key_column text;
    source_key_type_schema text;
    source_key_type_name text;
    source_text_column text;
    source_version_column text;
    locked_version bigint;
    locked_bytes bigint;
    locked_sha256 bytea;
BEGIN
    PERFORM pgcontext._consume_document_chunk_permit(
        25, p_document_source_id, p_source_version
    );
    SELECT rerank.source_schema_name, rerank.source_table_name,
           key_attribute.attname, rerank.source_key_type_schema,
           rerank.source_key_type_name, rerank.text_column_name,
           rerank.source_version_column_name
      INTO source_schema, source_table, source_key_column,
           source_key_type_schema, source_key_type_name,
           source_text_column, source_version_column
      FROM pgcontext._document_sources AS sources
      JOIN pgcontext._semantic_rerank_sources AS rerank USING (rerank_source_id)
      JOIN pg_catalog.pg_attribute AS key_attribute
        ON key_attribute.attrelid = rerank.source_table_oid
       AND key_attribute.attnum = rerank.source_key_attnum
       AND NOT key_attribute.attisdropped
     WHERE sources.document_source_id = p_document_source_id
       AND sources.status = 'ready' AND rerank.status = 'ready';
    IF source_key_column IS NULL THEN
        RETURN NULL;
    END IF;
    IF p_max_document_bytes NOT BETWEEN 1 AND 8388608
       OR p_source_bytes NOT BETWEEN 0 AND p_max_document_bytes THEN
        RETURN NULL;
    END IF;
    EXECUTE pg_catalog.format(
        'SELECT source.%1$I, pg_catalog.octet_length(source.%2$I)::bigint
           FROM %3$I.%4$I AS source
          WHERE source.%5$I = $1::text::%6$I.%7$I
          FOR SHARE OF source',
        source_version_column, source_text_column, source_schema, source_table,
        source_key_column, source_key_type_schema, source_key_type_name
    )
    INTO locked_version, locked_bytes
    USING p_source_key;
    IF locked_version IS DISTINCT FROM p_source_version
       OR locked_bytes IS DISTINCT FROM p_source_bytes
       OR locked_bytes > p_max_document_bytes THEN
        RETURN NULL;
    END IF;
    EXECUTE pg_catalog.format(
        'SELECT pg_catalog.sha256(
                    pg_catalog.convert_to(source.%1$I, ''UTF8'')
                )
           FROM %2$I.%3$I AS source
          WHERE source.%4$I = $1::text::%5$I.%6$I',
        source_text_column, source_schema, source_table, source_key_column,
        source_key_type_schema, source_key_type_name
    )
    INTO locked_sha256
    USING p_source_key;
    RETURN locked_sha256;
END;
$$;

CREATE FUNCTION pgcontext._lock_document_chunk_job_alias(
    p_job_id bigint, p_lease_token bigint, p_allow_ready_retained boolean
)
RETURNS boolean
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pgcontext
AS $$
DECLARE collection_id bigint; alias_matches boolean;
BEGIN
    PERFORM pgcontext._consume_document_chunk_permit(26, p_job_id, p_lease_token);
    SELECT sources.collection_id,
           generations.chunking_profile_id IN (
               aliases.chunking_profile_id,
               COALESCE(aliases.shadow_chunking_profile_id, aliases.chunking_profile_id)
           ) OR (
               p_allow_ready_retained AND jobs.status = 'ready' AND EXISTS (
                   SELECT 1
                     FROM pgcontext._chunking_profile_alias_retained AS retained
                    WHERE retained.chunking_profile_alias_id =
                              aliases.chunking_profile_alias_id
                      AND retained.chunking_profile_id = generations.chunking_profile_id
               )
           )
      INTO collection_id, alias_matches
      FROM pgcontext._document_chunk_jobs AS jobs
      JOIN pgcontext._document_chunk_generations AS generations USING (generation_id)
      JOIN pgcontext._document_sources AS sources USING (document_source_id)
      JOIN pgcontext._chunking_profile_aliases AS aliases
        USING (chunking_profile_alias_id)
     WHERE jobs.job_id = p_job_id
       AND (
           p_lease_token = 0
           OR (
               jobs.lease_token = p_lease_token
               AND (
                   jobs.lease_expires_at > pg_catalog.clock_timestamp()
                   OR jobs.status = 'ready'
               )
           )
       )
     FOR SHARE OF aliases;
    IF collection_id IS NULL THEN RETURN false; END IF;
    PERFORM pgcontext._require_collection_owner(collection_id);
    RETURN COALESCE(alias_matches, false);
END;
$$;

CREATE FUNCTION pgcontext._lock_document_chunk_read_alias(
    p_document_source_id bigint
)
RETURNS bigint
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pgcontext
AS $$
DECLARE collection_id bigint; profile_id bigint;
BEGIN
    PERFORM pgcontext._consume_document_chunk_permit(
        27, p_document_source_id, 0
    );
    SELECT sources.collection_id, aliases.chunking_profile_id
      INTO collection_id, profile_id
      FROM pgcontext._document_sources AS sources
      JOIN pgcontext._chunking_profile_aliases AS aliases
        USING (chunking_profile_alias_id)
     WHERE sources.document_source_id = p_document_source_id
       AND sources.status = 'ready' AND aliases.status = 'ready'
     FOR SHARE OF aliases;
    IF profile_id IS NULL THEN RETURN NULL; END IF;
    PERFORM pgcontext._require_collection_owner(collection_id);
    RETURN profile_id;
END;
$$;

CREATE FUNCTION pgcontext._checkpoint_document_chunk_job(
    p_job_id bigint, p_lease_token bigint, p_status text,
    p_processed_units bigint, p_total_units bigint
)
RETURNS boolean
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pgcontext
AS $$
DECLARE
    collection_id bigint;
    current_status text;
    current_processed bigint;
    current_total bigint;
    current_rank int4;
    next_rank int4;
BEGIN
    PERFORM pgcontext._consume_document_chunk_permit(9, p_job_id, p_lease_token);
    IF p_status NOT IN ('parsing','chunking','embedding')
       OR p_total_units NOT BETWEEN 1 AND 1000000
       OR p_processed_units NOT BETWEEN 0 AND p_total_units THEN
        RAISE EXCEPTION 'invalid document chunk progress checkpoint' USING ERRCODE = '22023';
    END IF;
    SELECT sources.collection_id, jobs.status, jobs.processed_units, jobs.total_units
      INTO collection_id, current_status, current_processed, current_total
      FROM pgcontext._document_chunk_jobs AS jobs
      JOIN pgcontext._document_chunk_generations AS generations USING (generation_id)
      JOIN pgcontext._document_sources AS sources USING (document_source_id)
     WHERE jobs.job_id = p_job_id AND jobs.lease_token = p_lease_token
       AND jobs.lease_expires_at > pg_catalog.clock_timestamp()
     FOR UPDATE OF jobs;
    PERFORM pgcontext._require_collection_owner(collection_id);
    current_rank := CASE current_status WHEN 'leased' THEN 0 WHEN 'parsing' THEN 1
        WHEN 'chunking' THEN 2 WHEN 'embedding' THEN 3 ELSE -1 END;
    next_rank := CASE p_status WHEN 'parsing' THEN 1 WHEN 'chunking' THEN 2 ELSE 3 END;
    IF current_rank < 0 OR next_rank < current_rank OR next_rank > current_rank + 1 THEN
        RAISE EXCEPTION 'invalid document chunk progress transition' USING ERRCODE = '55000';
    END IF;
    IF p_total_units < current_total OR p_processed_units < current_processed THEN
        RAISE EXCEPTION 'document chunk progress cannot regress' USING ERRCODE = '55000';
    END IF;
    UPDATE pgcontext._document_chunk_jobs SET status = p_status,
           processed_units = p_processed_units, total_units = p_total_units,
           updated_at = pg_catalog.now() WHERE job_id = p_job_id;
    UPDATE pgcontext._document_chunk_generations AS generations SET status = p_status
      FROM pgcontext._document_chunk_jobs AS jobs
     WHERE jobs.job_id = p_job_id AND generations.generation_id = jobs.generation_id;
    RETURN true;
END;
$$;

CREATE FUNCTION pgcontext._fail_document_chunk_job(
    p_job_id bigint, p_lease_token bigint, p_error_code text
)
RETURNS boolean
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pgcontext
AS $$
DECLARE collection_id bigint;
BEGIN
    PERFORM pgcontext._consume_document_chunk_permit(7, p_job_id, p_lease_token);
    IF pg_catalog.octet_length(p_error_code) NOT BETWEEN 1 AND 64
       OR p_error_code !~ '^[a-z0-9_]+$' THEN
        RAISE EXCEPTION 'invalid document chunk failure code' USING ERRCODE = '22023';
    END IF;
    SELECT sources.collection_id INTO collection_id
      FROM pgcontext._document_chunk_jobs AS jobs
      JOIN pgcontext._document_chunk_generations AS generations USING (generation_id)
      JOIN pgcontext._document_sources AS sources USING (document_source_id)
     WHERE jobs.job_id = p_job_id AND jobs.lease_token = p_lease_token
       AND jobs.lease_expires_at > pg_catalog.clock_timestamp()
     FOR UPDATE OF jobs;
    PERFORM pgcontext._require_collection_owner(collection_id);
    UPDATE pgcontext._document_chunk_jobs SET status = 'failed', lease_worker = NULL,
           lease_expires_at = NULL, error_code = p_error_code, updated_at = pg_catalog.now()
     WHERE job_id = p_job_id AND lease_token = p_lease_token
       AND status IN ('leased','parsing','chunking','embedding','validating','publishing');
    IF NOT FOUND THEN
        RAISE EXCEPTION 'stale document chunk job lease' USING ERRCODE = '40001';
    END IF;
    UPDATE pgcontext._document_chunk_generations AS generations SET status = 'failed'
      FROM pgcontext._document_chunk_jobs AS jobs
     WHERE jobs.job_id = p_job_id AND generations.generation_id = jobs.generation_id;
    DELETE FROM pgcontext._document_chunk_staging WHERE job_id = p_job_id;
    RETURN true;
END;
$$;

CREATE FUNCTION pgcontext._retry_document_chunk_job(p_job_id bigint)
RETURNS boolean
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pgcontext
AS $$
DECLARE collection_id bigint;
BEGIN
    PERFORM pgcontext._consume_document_chunk_permit(16, p_job_id, 0);
    SELECT sources.collection_id INTO collection_id
      FROM pgcontext._document_chunk_jobs AS jobs
      JOIN pgcontext._document_chunk_generations AS generations USING (generation_id)
      JOIN pgcontext._document_sources AS sources USING (document_source_id)
      JOIN pgcontext._chunking_profile_aliases AS aliases
        USING (chunking_profile_alias_id)
     WHERE jobs.job_id = p_job_id
       AND generations.chunking_profile_id IN (
           aliases.chunking_profile_id,
           COALESCE(aliases.shadow_chunking_profile_id, aliases.chunking_profile_id)
       )
     FOR UPDATE OF jobs FOR SHARE OF aliases;
    PERFORM pgcontext._require_collection_owner(collection_id);
    UPDATE pgcontext._document_chunk_jobs
       SET status = 'queued', lease_worker = NULL, lease_expires_at = NULL,
           error_code = NULL, processed_units = 0, updated_at = pg_catalog.now()
     WHERE job_id = p_job_id AND status IN ('cancelled','failed') AND attempt < 3;
    IF FOUND THEN
        DELETE FROM pgcontext._document_chunk_staging WHERE job_id = p_job_id;
        UPDATE pgcontext._document_chunk_generations AS generations
           SET status = 'queued'
          FROM pgcontext._document_chunk_jobs AS jobs
         WHERE jobs.job_id = p_job_id AND generations.generation_id = jobs.generation_id;
        RETURN true;
    END IF;
    RETURN false;
END;
$$;

CREATE FUNCTION pgcontext._supersede_document_chunk_claim(
    p_job_id bigint, p_lease_token bigint
)
RETURNS boolean
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pgcontext
AS $$
DECLARE collection_id bigint;
BEGIN
    PERFORM pgcontext._consume_document_chunk_permit(19, p_job_id, p_lease_token);
    SELECT sources.collection_id INTO collection_id
      FROM pgcontext._document_chunk_jobs AS jobs
      JOIN pgcontext._document_chunk_generations AS generations USING (generation_id)
      JOIN pgcontext._document_sources AS sources USING (document_source_id)
     WHERE jobs.job_id = p_job_id AND jobs.lease_token = p_lease_token
       AND jobs.lease_expires_at > pg_catalog.clock_timestamp()
     FOR SHARE OF sources;
    PERFORM pgcontext._require_collection_owner(collection_id);
    UPDATE pgcontext._document_chunk_jobs
       SET status = 'superseded', lease_worker = NULL, lease_expires_at = NULL,
           error_code = NULL, updated_at = pg_catalog.now()
     WHERE job_id = p_job_id AND lease_token = p_lease_token
       AND status IN ('leased','parsing','chunking','embedding','validating','publishing');
    IF NOT FOUND THEN
        RAISE EXCEPTION 'stale document chunk job lease' USING ERRCODE = '40001';
    END IF;
    UPDATE pgcontext._document_chunk_generations AS generations SET status = 'superseded'
      FROM pgcontext._document_chunk_jobs AS jobs
     WHERE jobs.job_id = p_job_id AND generations.generation_id = jobs.generation_id;
    DELETE FROM pgcontext._document_chunk_staging WHERE job_id = p_job_id;
    RETURN true;
END;
$$;

CREATE FUNCTION pgcontext._heartbeat_document_chunk_job(
    p_job_id bigint, p_lease_token bigint, p_lease_millis int4
)
RETURNS boolean
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pgcontext
AS $$
DECLARE collection_id bigint;
BEGIN
    PERFORM pgcontext._consume_document_chunk_permit(14, p_job_id, p_lease_token);
    IF p_lease_millis NOT BETWEEN 1 AND 60000 THEN
        RAISE EXCEPTION 'invalid document chunk heartbeat lease' USING ERRCODE = '22023';
    END IF;
    SELECT sources.collection_id INTO collection_id
      FROM pgcontext._document_chunk_jobs AS jobs
      JOIN pgcontext._document_chunk_generations AS generations USING (generation_id)
      JOIN pgcontext._document_sources AS sources USING (document_source_id)
     WHERE jobs.job_id = p_job_id;
    PERFORM pgcontext._require_collection_owner(collection_id);
    UPDATE pgcontext._document_chunk_jobs
       SET status = 'cancelled', lease_worker = NULL, lease_expires_at = NULL,
           error_code = 'cancelled', updated_at = pg_catalog.now()
     WHERE job_id = p_job_id AND lease_token = p_lease_token
       AND status = 'cancel_requested';
    IF FOUND THEN
        UPDATE pgcontext._document_chunk_generations AS generations SET status = 'cancelled'
          FROM pgcontext._document_chunk_jobs AS jobs
         WHERE jobs.job_id = p_job_id AND generations.generation_id = jobs.generation_id;
        RETURN false;
    END IF;
    UPDATE pgcontext._document_chunk_jobs
       SET lease_expires_at = pg_catalog.clock_timestamp()
               + pg_catalog.make_interval(secs => p_lease_millis::double precision / 1000.0),
           updated_at = pg_catalog.now()
     WHERE job_id = p_job_id AND lease_token = p_lease_token
       AND lease_expires_at > pg_catalog.clock_timestamp()
       AND status IN ('leased','parsing','chunking','embedding','validating','publishing');
    IF NOT FOUND THEN
        RAISE EXCEPTION 'stale document chunk job lease' USING ERRCODE = '40001';
    END IF;
    RETURN true;
END;
$$;

CREATE FUNCTION pgcontext._cancel_document_chunk_job(p_job_id bigint)
RETURNS boolean
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pgcontext
AS $$
DECLARE collection_id bigint;
BEGIN
    PERFORM pgcontext._consume_document_chunk_permit(15, p_job_id, 0);
    SELECT sources.collection_id INTO collection_id
      FROM pgcontext._document_chunk_jobs AS jobs
      JOIN pgcontext._document_chunk_generations AS generations USING (generation_id)
      JOIN pgcontext._document_sources AS sources USING (document_source_id)
     WHERE jobs.job_id = p_job_id;
    PERFORM pgcontext._require_collection_owner(collection_id);
    UPDATE pgcontext._document_chunk_jobs
       SET status = CASE WHEN status = 'queued' THEN 'cancelled' ELSE 'cancel_requested' END,
           lease_worker = CASE WHEN status = 'queued' THEN NULL ELSE lease_worker END,
           lease_expires_at = CASE WHEN status = 'queued' THEN NULL ELSE lease_expires_at END,
           error_code = 'cancelled', updated_at = pg_catalog.now()
     WHERE job_id = p_job_id
       AND status IN ('queued','leased','parsing','chunking','embedding','validating','publishing');
    IF NOT FOUND THEN RETURN false; END IF;
    DELETE FROM pgcontext._document_chunk_staging WHERE job_id = p_job_id;
    UPDATE pgcontext._document_chunk_generations AS generations SET status = jobs.status
      FROM pgcontext._document_chunk_jobs AS jobs
     WHERE jobs.job_id = p_job_id AND generations.generation_id = jobs.generation_id;
    RETURN true;
END;
$$;

CREATE FUNCTION pgcontext._rebuild_document_chunk_job(p_job_id bigint)
RETURNS boolean
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pgcontext
AS $$
DECLARE v_collection_id bigint; v_generation_id bigint;
BEGIN
    PERFORM pgcontext._consume_document_chunk_permit(18, p_job_id, 0);
    SELECT sources.collection_id, jobs.generation_id
      INTO v_collection_id, v_generation_id
      FROM pgcontext._document_chunk_jobs AS jobs
      JOIN pgcontext._document_chunk_generations AS generations USING (generation_id)
      JOIN pgcontext._document_sources AS sources USING (document_source_id)
      JOIN pgcontext._chunking_profile_aliases AS aliases
        USING (chunking_profile_alias_id)
     WHERE jobs.job_id = p_job_id AND jobs.status IN ('ready','retired')
       AND generations.chunking_profile_id IN (
           aliases.chunking_profile_id,
           COALESCE(aliases.shadow_chunking_profile_id, aliases.chunking_profile_id)
       )
     FOR UPDATE OF jobs FOR SHARE OF aliases;
    PERFORM pgcontext._require_collection_owner(v_collection_id);
    IF v_generation_id IS NULL THEN RETURN false; END IF;
    DELETE FROM pgcontext._current_document_chunk_generations
     WHERE generation_id = v_generation_id;
    DELETE FROM pgcontext._document_embedding_jobs
     WHERE generation_id = v_generation_id;
    DELETE FROM pgcontext._document_chunk_staging WHERE job_id = p_job_id;
    UPDATE pgcontext._document_chunk_generations
       SET status = 'queued', chunk_count = NULL, token_count = NULL,
           staging_bytes = NULL, publication_sha256 = NULL,
           projection_sha256 = NULL, published_at = NULL
     WHERE generation_id = v_generation_id;
    UPDATE pgcontext._document_chunk_jobs
       SET status = 'queued', attempt = 0, lease_worker = NULL,
           lease_expires_at = NULL, processed_units = 0, error_code = NULL,
           updated_at = pg_catalog.now()
     WHERE job_id = p_job_id;
    RETURN true;
END;
$$;
"#,
    name = "create_document_chunking_lifecycle_helpers",
    requires = [
        "create_document_chunking_catalog_tables",
        "create_document_chunking_profile_state"
    ]
);
