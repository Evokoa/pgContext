//! Logical-dump registration for durable P13 configuration catalogs.

pgrx::extension_sql!(
    r#"
CREATE INDEX _document_chunk_jobs_claimable_idx
    ON pgcontext._document_chunk_jobs (updated_at, job_id)
    WHERE status IN (
        'queued','leased','parsing','chunking','embedding','validating',
        'publishing','cancel_requested'
    );
CREATE INDEX _document_chunk_generations_lifecycle_idx
    ON pgcontext._document_chunk_generations (
        document_source_id, source_key, chunking_profile_id, status, generation_id
    );
SELECT pg_catalog.pg_extension_config_dump('pgcontext._chunking_profiles', '');
SELECT pg_catalog.pg_extension_config_dump('pgcontext._chunking_profiles_chunking_profile_id_seq', '');
SELECT pg_catalog.pg_extension_config_dump('pgcontext._chunking_profile_aliases', '');
SELECT pg_catalog.pg_extension_config_dump('pgcontext._chunking_profile_aliases_chunking_profile_alias_id_seq', '');
SELECT pg_catalog.pg_extension_config_dump('pgcontext._chunking_profile_alias_history', '');
SELECT pg_catalog.pg_extension_config_dump('pgcontext._document_sources', '');
SELECT pg_catalog.pg_extension_config_dump('pgcontext._document_sources_document_source_id_seq', '');
"#,
    name = "register_document_chunking_config_dump",
    requires = ["create_document_chunking_catalog_tables"]
);
