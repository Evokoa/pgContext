//! Immutable profile registration helper for automatic document chunking.

pgrx::extension_sql!(
    r#"
CREATE FUNCTION pgcontext._register_chunking_profile(
    p_profile_name text,
    p_parser_revision text,
    p_target_tokens int4,
    p_max_tokens int4,
    p_min_tokens int4,
    p_overlap_tokens int4,
    p_max_document_bytes bigint,
    p_include_structure_context boolean,
    p_configuration_sha256 bytea
)
RETURNS bigint
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pgcontext
AS $$
DECLARE
    existing pgcontext._chunking_profiles%ROWTYPE;
    profile_id bigint;
    alias_id bigint;
BEGIN
    PERFORM pgcontext._consume_document_chunk_permit(3, 0, 0);
    PERFORM pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtextextended(
        SESSION_USER::text || E'\x1fprofile\x1f' || p_profile_name, 0
    ));
    SELECT * INTO existing
      FROM pgcontext._chunking_profiles
     WHERE owner_role = SESSION_USER::pg_catalog.regrole::oid
       AND profile_name = p_profile_name;
    IF FOUND THEN
        IF existing.parser_revision = p_parser_revision
           AND existing.target_tokens = p_target_tokens
           AND existing.max_tokens = p_max_tokens
           AND existing.min_tokens = p_min_tokens
           AND existing.overlap_tokens = p_overlap_tokens
           AND existing.max_document_bytes = p_max_document_bytes
           AND existing.include_structure_context = p_include_structure_context
           AND existing.configuration_sha256 = p_configuration_sha256
           AND existing.status = 'ready' THEN
            profile_id := existing.chunking_profile_id;
        ELSE
            RAISE EXCEPTION 'chunking profile name already identifies a different immutable contract'
                USING ERRCODE = '42710';
        END IF;
    ELSE
        INSERT INTO pgcontext._chunking_profiles (
            owner_role, profile_name, parser_revision, target_tokens, max_tokens,
            min_tokens, overlap_tokens, max_document_bytes,
            include_structure_context, configuration_sha256
        ) VALUES (
            SESSION_USER::pg_catalog.regrole::oid, p_profile_name, p_parser_revision,
            p_target_tokens, p_max_tokens, p_min_tokens, p_overlap_tokens,
            p_max_document_bytes, p_include_structure_context, p_configuration_sha256
        ) RETURNING chunking_profile_id INTO profile_id;
        UPDATE pgcontext._chunking_profiles
           SET profile_revision = profile_id
         WHERE chunking_profile_id = profile_id;
    END IF;
    INSERT INTO pgcontext._chunking_profile_aliases (
        owner_role, alias_name, chunking_profile_id
    ) VALUES (
        SESSION_USER::pg_catalog.regrole::oid, p_profile_name, profile_id
    )
    ON CONFLICT (owner_role, alias_name) DO NOTHING
    RETURNING chunking_profile_alias_id INTO alias_id;
    IF alias_id IS NULL THEN
        SELECT aliases.chunking_profile_alias_id INTO alias_id
          FROM pgcontext._chunking_profile_aliases AS aliases
         WHERE aliases.owner_role = SESSION_USER::pg_catalog.regrole::oid
           AND aliases.alias_name = p_profile_name;
    END IF;
    INSERT INTO pgcontext._chunking_profile_alias_history (
        chunking_profile_alias_id, alias_revision, chunking_profile_id
    ) VALUES (alias_id, 1, profile_id)
    ON CONFLICT DO NOTHING;
    RETURN profile_id;
END;
$$;

CREATE FUNCTION pgcontext._promote_chunking_profile_alias(
    p_alias_id bigint,
    p_profile_id bigint
)
RETURNS bigint
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pgcontext
AS $$
DECLARE
    alias_row pgcontext._chunking_profile_aliases%ROWTYPE;
    next_revision bigint;
BEGIN
    PERFORM pgcontext._consume_document_chunk_permit(20, p_alias_id, p_profile_id);
    SELECT * INTO alias_row FROM pgcontext._chunking_profile_aliases
     WHERE chunking_profile_alias_id = p_alias_id FOR UPDATE;
    IF NOT FOUND OR NOT pg_catalog.pg_has_role(SESSION_USER, alias_row.owner_role, 'MEMBER')
       OR NOT EXISTS (
           SELECT 1 FROM pgcontext._chunking_profiles AS profiles
            WHERE profiles.chunking_profile_id = p_profile_id
              AND profiles.status = 'ready'
              AND pg_catalog.pg_has_role(SESSION_USER, profiles.owner_role, 'MEMBER')
       ) THEN
        RAISE EXCEPTION 'chunking profile alias or target is unavailable'
            USING ERRCODE = '42704';
    END IF;
    IF alias_row.chunking_profile_id = p_profile_id THEN
        RETURN alias_row.alias_revision;
    END IF;
    IF alias_row.shadow_chunking_profile_id IS DISTINCT FROM p_profile_id THEN
        RAISE EXCEPTION 'chunking profile target is not prepared as shadow'
            USING ERRCODE = '55000';
    END IF;
    next_revision := alias_row.alias_revision + 1;
    IF NOT EXISTS (
           SELECT 1 FROM pgcontext._chunking_profile_alias_retained
            WHERE chunking_profile_alias_id = p_alias_id
              AND chunking_profile_id = alias_row.chunking_profile_id
       ) AND (SELECT pg_catalog.count(*)
                FROM pgcontext._chunking_profile_alias_retained
               WHERE chunking_profile_alias_id = p_alias_id
                 AND chunking_profile_id <> p_profile_id) >= 8 THEN
        RAISE EXCEPTION 'chunking profile alias retained-profile limit reached'
            USING ERRCODE = '54000';
    END IF;
    INSERT INTO pgcontext._chunking_profile_alias_retained (
        chunking_profile_alias_id, chunking_profile_id, retained_revision
    ) VALUES (p_alias_id, alias_row.chunking_profile_id, next_revision)
    ON CONFLICT (chunking_profile_alias_id, chunking_profile_id) DO UPDATE
       SET retained_revision = EXCLUDED.retained_revision,
           retained_at = pg_catalog.now();
    DELETE FROM pgcontext._chunking_profile_alias_retained
     WHERE chunking_profile_alias_id = p_alias_id
       AND chunking_profile_id = p_profile_id;
    UPDATE pgcontext._chunking_profile_aliases
       SET chunking_profile_id = p_profile_id,
           shadow_chunking_profile_id = NULL,
           alias_revision = next_revision,
           updated_at = pg_catalog.now()
     WHERE chunking_profile_alias_id = p_alias_id;
    INSERT INTO pgcontext._chunking_profile_alias_history (
        chunking_profile_alias_id, alias_revision, chunking_profile_id
    ) VALUES (p_alias_id, next_revision, p_profile_id);
    RETURN next_revision;
END;
$$;

CREATE FUNCTION pgcontext._rollback_chunking_profile_alias(p_alias_id bigint)
RETURNS bigint
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pgcontext
AS $$
DECLARE
    alias_row pgcontext._chunking_profile_aliases%ROWTYPE;
    target_profile_id bigint;
    next_revision bigint;
BEGIN
    PERFORM pgcontext._consume_document_chunk_permit(21, p_alias_id, 0);
    SELECT * INTO alias_row FROM pgcontext._chunking_profile_aliases
     WHERE chunking_profile_alias_id = p_alias_id FOR UPDATE;
    IF NOT FOUND OR NOT pg_catalog.pg_has_role(SESSION_USER, alias_row.owner_role, 'MEMBER') THEN
        RAISE EXCEPTION 'chunking profile alias is unavailable' USING ERRCODE = '42704';
    END IF;
    SELECT retained.chunking_profile_id INTO target_profile_id
      FROM pgcontext._chunking_profile_alias_retained AS retained
     WHERE retained.chunking_profile_alias_id = p_alias_id
     ORDER BY retained.retained_revision DESC LIMIT 1;
    IF target_profile_id IS NULL THEN
        RAISE EXCEPTION 'chunking profile alias has no rollback target'
            USING ERRCODE = '55000';
    END IF;
    next_revision := alias_row.alias_revision + 1;
    INSERT INTO pgcontext._chunking_profile_alias_retained (
        chunking_profile_alias_id, chunking_profile_id, retained_revision
    ) VALUES (p_alias_id, alias_row.chunking_profile_id, next_revision)
    ON CONFLICT (chunking_profile_alias_id, chunking_profile_id) DO UPDATE
       SET retained_revision = EXCLUDED.retained_revision,
           retained_at = pg_catalog.now();
    DELETE FROM pgcontext._chunking_profile_alias_retained
     WHERE chunking_profile_alias_id = p_alias_id
       AND chunking_profile_id = target_profile_id;
    UPDATE pgcontext._chunking_profile_aliases
       SET chunking_profile_id = target_profile_id,
           shadow_chunking_profile_id = NULL,
           alias_revision = next_revision,
           updated_at = pg_catalog.now()
     WHERE chunking_profile_alias_id = p_alias_id;
    INSERT INTO pgcontext._chunking_profile_alias_history (
        chunking_profile_alias_id, alias_revision, chunking_profile_id
    ) VALUES (p_alias_id, next_revision, target_profile_id);
    RETURN next_revision;
END;
$$;

CREATE FUNCTION pgcontext._prepare_chunking_profile_alias(
    p_alias_id bigint,
    p_profile_id bigint
)
RETURNS bigint
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pgcontext
AS $$
DECLARE alias_row pgcontext._chunking_profile_aliases%ROWTYPE;
BEGIN
    PERFORM pgcontext._consume_document_chunk_permit(22, p_alias_id, p_profile_id);
    SELECT * INTO alias_row FROM pgcontext._chunking_profile_aliases
     WHERE chunking_profile_alias_id = p_alias_id FOR UPDATE;
    IF NOT FOUND OR NOT pg_catalog.pg_has_role(SESSION_USER, alias_row.owner_role, 'MEMBER')
       OR NOT EXISTS (
           SELECT 1 FROM pgcontext._chunking_profiles AS profiles
            WHERE profiles.chunking_profile_id = p_profile_id
              AND profiles.owner_role = alias_row.owner_role
              AND profiles.status = 'ready'
       ) THEN
        RAISE EXCEPTION 'chunking profile alias or shadow target is unavailable'
            USING ERRCODE = '42704';
    END IF;
    IF p_profile_id = alias_row.chunking_profile_id THEN
        RAISE EXCEPTION 'chunking profile shadow must differ from current target'
            USING ERRCODE = '22023';
    END IF;
    UPDATE pgcontext._chunking_profile_aliases
       SET shadow_chunking_profile_id = p_profile_id, updated_at = pg_catalog.now()
     WHERE chunking_profile_alias_id = p_alias_id;
    RETURN alias_row.alias_revision;
END;
$$;

CREATE FUNCTION pgcontext._drain_chunking_profile_alias(
    p_alias_id bigint,
    p_profile_id bigint
)
RETURNS boolean
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pgcontext
AS $$
DECLARE alias_row pgcontext._chunking_profile_aliases%ROWTYPE;
BEGIN
    PERFORM pgcontext._consume_document_chunk_permit(23, p_alias_id, p_profile_id);
    SELECT * INTO alias_row FROM pgcontext._chunking_profile_aliases
     WHERE chunking_profile_alias_id = p_alias_id FOR UPDATE;
    IF NOT FOUND OR NOT pg_catalog.pg_has_role(SESSION_USER, alias_row.owner_role, 'MEMBER') THEN
        RAISE EXCEPTION 'chunking profile alias is unavailable' USING ERRCODE = '42704';
    END IF;
    IF p_profile_id IN (
        alias_row.chunking_profile_id,
        COALESCE(alias_row.shadow_chunking_profile_id, alias_row.chunking_profile_id)
    ) THEN
        RAISE EXCEPTION 'current or prepared chunking profile cannot be drained'
            USING ERRCODE = '55000';
    END IF;
    DELETE FROM pgcontext._chunking_profile_alias_retained
     WHERE chunking_profile_alias_id = p_alias_id
       AND chunking_profile_id = p_profile_id;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'chunking profile is not retained by this alias'
            USING ERRCODE = '55000';
    END IF;
    RETURN true;
END;
$$;
"#,
    name = "create_document_chunking_profile_helper",
    requires = ["create_document_chunking_profile_state"]
);
