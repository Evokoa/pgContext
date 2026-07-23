#[pg_test]
fn default_privileges_require_explicit_api_grants_for_fresh_roles() {
    default_privileges_create_role("m1_default_priv_probe");

    assert!(
        !default_privileges_has_schema_privilege("m1_default_priv_probe", "pgcontext", "USAGE"),
        "fresh roles must not receive pgcontext schema USAGE implicitly"
    );
    assert!(
        default_privileges_has_type_privilege("m1_default_priv_probe", "vector", "USAGE"),
        "fresh roles should retain PostgreSQL's default type USAGE on vector"
    );

    let functions_without_execute =
        default_privileges_functions_without_execute("m1_default_priv_probe");
    assert!(
        functions_without_execute.is_empty(),
        "fresh roles should retain PostgreSQL's default EXECUTE on pgcontext functions: {functions_without_execute:?}"
    );
}

#[pg_test]
fn default_privileges_protect_extension_catalog_tables_and_sequences() {
    default_privileges_create_role("m1_default_priv_catalog_probe");

    let table_privileges = default_privileges_catalog_table_privileges(
        "m1_default_priv_catalog_probe",
        &["SELECT", "INSERT", "UPDATE", "DELETE", "TRUNCATE", "REFERENCES", "TRIGGER"],
    );
    assert!(
        table_privileges.is_empty(),
        "fresh roles must not receive privileges on extension-owned catalog tables: {table_privileges:?}"
    );

    let sequence_privileges = default_privileges_catalog_sequence_privileges(
        "m1_default_priv_catalog_probe",
        &["SELECT", "UPDATE", "USAGE"],
    );
    assert!(
        sequence_privileges.is_empty(),
        "fresh roles must not receive privileges on extension-owned identity sequences: {sequence_privileges:?}"
    );
}

#[pg_test]
fn default_privileges_keep_acl_visibility_views_selectable() {
    default_privileges_create_role("m1_default_priv_view_probe");

    let missing_select_grants =
        default_privileges_visibility_views_without_select("m1_default_priv_view_probe");
    assert!(
        missing_select_grants.is_empty(),
        "fresh roles must retain SELECT grants on ACL-filtered visibility views: {missing_select_grants:?}"
    );
}

#[pg_test]
fn membership_filtered_visibility_views_are_security_barriers() {
    let unbarriered_views = default_privileges_names_from_query(
        "SELECT relname::text
           FROM pg_catalog.pg_class
           JOIN pg_catalog.pg_namespace
             ON pg_namespace.oid = pg_class.relnamespace
          WHERE pg_namespace.nspname = 'pgcontext'
            AND relkind = 'v'
            AND pg_catalog.pg_get_viewdef(pg_class.oid)
                ILIKE '%pg_has_role(SESSION_USER,%'
            AND NOT COALESCE(
                reloptions @> ARRAY['security_barrier=true']::text[],
                false
            )
          ORDER BY 1",
        "visibility-view security-barrier query should succeed",
    );
    assert!(
        unbarriered_views.is_empty(),
        "every membership-filtered visibility view must be a security barrier: {unbarriered_views:?}"
    );
}

#[pg_test]
fn default_privileges_catalog_operators_and_opclasses_are_schema_bound() {
    let misplaced_operators = default_privileges_names_from_query(
        "SELECT oprname::text
           FROM pg_catalog.pg_operator
          WHERE oprname IN ('<->', '<#>', '<=>', '<+>', '<~>', '<%>', '<', '<=', '=', '<>', '>=', '>')
            AND (
                oprleft IN ('vector'::regtype, 'halfvec'::regtype, 'sparsevec'::regtype, 'bitvec'::regtype)
                OR oprright IN ('vector'::regtype, 'halfvec'::regtype, 'sparsevec'::regtype, 'bitvec'::regtype)
            )
            AND oprnamespace <> 'pgcontext'::regnamespace
          ORDER BY 1",
        "operator namespace query should succeed",
    );
    assert!(
        misplaced_operators.is_empty(),
        "pgContext operators must be installed only in the extension schema: {misplaced_operators:?}"
    );

    let misplaced_opclasses = default_privileges_names_from_query(
        "SELECT opcname::text
           FROM pg_catalog.pg_opclass
          WHERE opcname IN ('vector_ops', 'halfvec_ops', 'sparsevec_ops', 'bitvec_ops', 'vector_hnsw_ops', 'vector_hnsw_ip_ops', 'vector_hnsw_cosine_ops', 'vector_hnsw_l1_ops', 'halfvec_hnsw_ops', 'halfvec_hnsw_ip_ops', 'halfvec_hnsw_cosine_ops', 'halfvec_hnsw_l1_ops', 'sparsevec_hnsw_ops', 'sparsevec_hnsw_ip_ops', 'sparsevec_hnsw_cosine_ops', 'sparsevec_hnsw_l1_ops', 'bitvec_hnsw_hamming_ops', 'bitvec_hnsw_jaccard_ops')
            AND opcintype IN ('vector'::regtype, 'halfvec'::regtype, 'sparsevec'::regtype, 'bitvec'::regtype)
            AND opcnamespace <> 'pgcontext'::regnamespace
          ORDER BY 1",
        "operator class namespace query should succeed",
    );
    assert!(
        misplaced_opclasses.is_empty(),
        "pgContext operator classes must be installed only in the extension schema: {misplaced_opclasses:?}"
    );
}

fn default_privileges_create_role(role_name: &str) {
    Spi::run(&format!("CREATE ROLE {role_name}")).expect("role should be created");
}

fn default_privileges_has_schema_privilege(
    role_name: &str,
    schema_name: &str,
    privilege: &str,
) -> bool {
    default_privileges_bool_from_query(
        "SELECT pg_catalog.has_schema_privilege($1, $2, $3)",
        &[role_name.into(), schema_name.into(), privilege.into()],
        "schema privilege query should succeed",
    )
}

fn default_privileges_has_type_privilege(
    role_name: &str,
    type_name: &str,
    privilege: &str,
) -> bool {
    default_privileges_bool_from_query(
        "SELECT pg_catalog.has_type_privilege($1, $2, $3)",
        &[role_name.into(), type_name.into(), privilege.into()],
        "type privilege query should succeed",
    )
}

fn default_privileges_functions_without_execute(role_name: &str) -> Vec<String> {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT proname::text
                    || '('
                    || pg_catalog.pg_get_function_identity_arguments(pg_proc.oid)::text
                    || ')'
               FROM pg_catalog.pg_proc
               JOIN pg_catalog.pg_namespace
                 ON pg_namespace.oid = pg_proc.pronamespace
              WHERE pg_namespace.nspname = 'pgcontext'
                AND NOT pg_catalog.has_function_privilege($1, pg_proc.oid, 'EXECUTE')
              ORDER BY 1",
            Some(1),
            &[role_name.into()],
        )?;

        let mut missing = Vec::new();
        for row in rows {
            missing.push(
                row.get::<String>(1)?
                    .expect("function identity should not be null"),
            );
        }

        Ok::<_, spi::Error>(missing)
    })
    .expect("function privilege query should succeed")
}

fn default_privileges_catalog_table_privileges(
    role_name: &str,
    privileges: &[&str],
) -> Vec<String> {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT relname::text, requested.privilege::text
               FROM pg_catalog.pg_class
               JOIN pg_catalog.pg_namespace
                 ON pg_namespace.oid = pg_class.relnamespace
              CROSS JOIN unnest($2::text[]) AS requested(privilege)
              WHERE pg_namespace.nspname = 'pgcontext'
                AND relkind = 'r'
                AND pg_catalog.has_table_privilege($1, pg_class.oid, requested.privilege)
              ORDER BY 1, 2",
            Some(2),
            &[role_name.into(), privileges.into()],
        )?;

        let mut granted = Vec::new();
        for row in rows {
            let relname = row.get::<String>(1)?.expect("relation name should not be null");
            let privilege = row.get::<String>(2)?.expect("privilege should not be null");
            granted.push(format!("{relname}:{privilege}"));
        }

        Ok::<_, spi::Error>(granted)
    })
    .expect("catalog table privilege query should succeed")
}

fn default_privileges_catalog_sequence_privileges(
    role_name: &str,
    privileges: &[&str],
) -> Vec<String> {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT relname::text, requested.privilege::text
               FROM pg_catalog.pg_class
               JOIN pg_catalog.pg_namespace
                 ON pg_namespace.oid = pg_class.relnamespace
              CROSS JOIN unnest($2::text[]) AS requested(privilege)
              WHERE pg_namespace.nspname = 'pgcontext'
                AND relkind = 'S'
                AND pg_catalog.has_sequence_privilege($1, pg_class.oid, requested.privilege)
              ORDER BY 1, 2",
            Some(2),
            &[role_name.into(), privileges.into()],
        )?;

        let mut granted = Vec::new();
        for row in rows {
            let relname = row.get::<String>(1)?.expect("sequence name should not be null");
            let privilege = row.get::<String>(2)?.expect("privilege should not be null");
            granted.push(format!("{relname}:{privilege}"));
        }

        Ok::<_, spi::Error>(granted)
    })
    .expect("catalog sequence privilege query should succeed")
}

fn default_privileges_visibility_views_without_select(role_name: &str) -> Vec<String> {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT relname::text
               FROM pg_catalog.pg_class
               JOIN pg_catalog.pg_namespace
                 ON pg_namespace.oid = pg_class.relnamespace
              WHERE pg_namespace.nspname = 'pgcontext'
                AND relkind = 'v'
                AND relname IN (
                    '_collection_acl',
                    '_visible_artifact_segments',
                    '_visible_build_jobs',
                    '_visible_collection_late_interaction',
                    '_visible_collection_limits',
                    '_visible_collection_payload_columns',
                    '_visible_collection_points',
                    '_visible_collection_sparse_vectors',
                    '_visible_collection_vectors',
                    '_visible_collections',
                    '_visible_pgvector_ownership_conversions',
                    '_visible_query_stats'
                )
                AND NOT pg_catalog.has_table_privilege($1, pg_class.oid, 'SELECT')
              ORDER BY 1",
            Some(1),
            &[role_name.into()],
        )?;

        let mut missing = Vec::new();
        for row in rows {
            missing.push(
                row.get::<String>(1)?
                    .expect("view name should not be null"),
            );
        }

        Ok::<_, spi::Error>(missing)
    })
    .expect("visibility view privilege query should succeed")
}

fn default_privileges_names_from_query(query: &str, context: &str) -> Vec<String> {
    Spi::connect(|client| {
        let rows = client.select(query, None, &[])?;
        let mut names = Vec::new();
        for row in rows {
            names.push(
                row.get::<String>(1)?
                    .expect("name should not be null"),
            );
        }

        Ok::<_, spi::Error>(names)
    })
    .expect(context)
}

fn default_privileges_bool_from_query(
    query: &str,
    args: &[pgrx::datum::DatumWithOid<'_>],
    context: &str,
) -> bool {
    Spi::connect(|client| {
        let rows = client.select(query, Some(args.len() as i64), args)?;
        assert!(!rows.is_empty(), "query should return one row");
        let row = rows.first();

        Ok::<_, spi::Error>(row.get::<bool>(1)?.expect("value should not be null"))
    })
    .expect(context)
}
