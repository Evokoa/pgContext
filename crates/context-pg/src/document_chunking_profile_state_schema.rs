//! Bounded draining-profile state for P13 profile aliases.

pgrx::extension_sql!(
    r#"
CREATE TABLE pgcontext._chunking_profile_alias_retained (
    chunking_profile_alias_id bigint NOT NULL
        REFERENCES pgcontext._chunking_profile_aliases(chunking_profile_alias_id)
        ON DELETE CASCADE,
    chunking_profile_id bigint NOT NULL
        REFERENCES pgcontext._chunking_profiles(chunking_profile_id),
    retained_revision bigint NOT NULL CHECK (retained_revision > 0),
    retained_at timestamptz NOT NULL DEFAULT pg_catalog.now(),
    PRIMARY KEY (chunking_profile_alias_id, chunking_profile_id)
);
REVOKE ALL ON TABLE pgcontext._chunking_profile_alias_retained FROM PUBLIC;
CREATE VIEW pgcontext._visible_chunking_profile_alias_retained
WITH (security_barrier = true) AS
SELECT retained.*
  FROM pgcontext._chunking_profile_alias_retained AS retained
  JOIN pgcontext._chunking_profile_aliases AS aliases USING (chunking_profile_alias_id)
 WHERE pg_catalog.pg_has_role(SESSION_USER, aliases.owner_role, 'MEMBER');
GRANT SELECT ON pgcontext._visible_chunking_profile_alias_retained TO PUBLIC;
SELECT pg_catalog.pg_extension_config_dump(
    'pgcontext._chunking_profile_alias_retained', ''
);
"#,
    name = "create_document_chunking_profile_state",
    requires = ["create_document_chunking_catalog_tables"]
);
