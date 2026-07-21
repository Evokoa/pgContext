#[pg_test]
fn installed_pgcontext_functions_are_classified_in_contract_registry() {
    let installed = installed_pgcontext_functions();
    assert!(
        !installed.is_empty(),
        "expected at least one installed pgcontext function"
    );

    let registered = crate::contract::SQL_CONTRACT_OBJECTS
        .iter()
        .filter(|object| object.kind == crate::contract::SqlObjectKind::Function)
        .map(|object| {
            (
                object.name.to_owned(),
                object.identity.unwrap_or_default().to_owned(),
            )
        })
        .collect::<std::collections::BTreeSet<_>>();

    let missing = installed
        .iter()
        .filter(|function| !registered.contains(*function))
        .collect::<Vec<_>>();
    let stale = registered
        .iter()
        .filter(|function| !installed.contains(*function))
        .collect::<Vec<_>>();

    assert!(
        missing.is_empty(),
        "installed pgcontext functions missing lifecycle metadata: {missing:?}"
    );
    assert!(
        stale.is_empty(),
        "function lifecycle metadata has no installed object: {stale:?}"
    );
}

#[pg_test]
fn stable_contract_registry_marks_search_and_query_as_distinct_surfaces() {
    let stable_functions = crate::contract::SQL_CONTRACT_OBJECTS
        .iter()
        .filter(|object| {
            object.kind == crate::contract::SqlObjectKind::Function
                && object.lifecycle == crate::contract::SqlLifecycle::Stable
        })
        .map(|object| {
            (
                object.name.to_owned(),
                object.identity.unwrap_or_default().to_owned(),
            )
        })
        .collect::<std::collections::BTreeSet<_>>();

    assert!(stable_functions.contains(&(
        "search".to_owned(),
        "collection text, vector vector, \"limit\" integer".to_owned(),
    )));
    assert!(stable_functions.contains(&(
        "search".to_owned(),
        "collection text, vector vector, filter text, \"limit\" integer".to_owned(),
    )));
    assert!(stable_functions.contains(&(
        "search".to_owned(),
        "collection text, vector vector, filter text, candidate_point_ids bigint[], \"limit\" integer"
            .to_owned(),
    )));
    assert!(stable_functions.contains(&(
        "query".to_owned(),
        "collection text, vector vector, text_query text, text_column text, \"limit\" integer"
            .to_owned(),
    )));
    assert!(stable_functions.contains(&(
        "count".to_owned(),
        "collection text".to_owned(),
    )));
    assert!(stable_functions.contains(&(
        "count".to_owned(),
        "collection text, filter text".to_owned(),
    )));
}

#[pg_test]
fn installed_pgcontext_objects_are_classified_in_contract_registry() {
    assert_registered_objects_cover_installed(
        crate::contract::SqlObjectKind::AccessMethod,
        installed_access_methods(),
    );
    assert_registered_objects_cover_installed(
        crate::contract::SqlObjectKind::Aggregate,
        installed_aggregates(),
    );
    assert_registered_objects_cover_installed(crate::contract::SqlObjectKind::Cast, installed_casts());
    assert_registered_objects_cover_installed(
        crate::contract::SqlObjectKind::Operator,
        installed_operators(),
    );
    assert_registered_objects_cover_installed(
        crate::contract::SqlObjectKind::OperatorClass,
        installed_operator_classes(),
    );
    assert_registered_objects_cover_installed(
        crate::contract::SqlObjectKind::Schema,
        installed_schemas(),
    );
    assert_registered_objects_cover_installed(
        crate::contract::SqlObjectKind::Table,
        installed_relations("r"),
    );
    assert_registered_objects_cover_installed(
        crate::contract::SqlObjectKind::Trigger,
        installed_triggers(),
    );
    assert_registered_objects_cover_installed(
        crate::contract::SqlObjectKind::Type,
        installed_contract_types(),
    );
    assert_registered_objects_cover_installed(
        crate::contract::SqlObjectKind::View,
        installed_relations("v"),
    );
}

fn assert_registered_objects_cover_installed(
    kind: crate::contract::SqlObjectKind,
    installed: std::collections::BTreeSet<(String, String)>,
) {
    assert!(
        !installed.is_empty(),
        "expected installed objects for kind {kind:?}"
    );

    let registered = crate::contract::SQL_CONTRACT_OBJECTS
        .iter()
        .filter(|object| object.kind == kind)
        .map(|object| {
            (
                object.name.to_owned(),
                object.identity.unwrap_or_default().to_owned(),
            )
        })
        .collect::<std::collections::BTreeSet<_>>();

    let missing = installed
        .iter()
        .filter(|object| !registered.contains(*object))
        .collect::<Vec<_>>();
    let stale = registered
        .iter()
        .filter(|object| !installed.contains(*object))
        .collect::<Vec<_>>();

    assert!(
        missing.is_empty(),
        "installed {kind:?} objects missing lifecycle metadata: {missing:?}"
    );
    assert!(
        stale.is_empty(),
        "{kind:?} lifecycle metadata has no installed object: {stale:?}; installed: {installed:?}"
    );
}

fn installed_pgcontext_functions() -> std::collections::BTreeSet<(String, String)> {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT proname::text,
                    pg_catalog.pg_get_function_identity_arguments(pg_proc.oid)::text
               FROM pg_catalog.pg_proc
               JOIN pg_catalog.pg_namespace
                 ON pg_namespace.oid = pg_proc.pronamespace
              WHERE pg_namespace.nspname = 'pgcontext'
              ORDER BY 1, 2",
            None,
            &[],
        )?;

        let mut functions = std::collections::BTreeSet::new();
        for row in rows {
            functions.insert((
                row.get::<String>(1)?.expect("function name should not be null"),
                row.get::<String>(2)?
                    .expect("function identity should not be null"),
            ));
        }

        Ok::<_, spi::Error>(functions)
    })
    .expect("installed function query should succeed")
}

fn installed_access_methods() -> std::collections::BTreeSet<(String, String)> {
    names_from_query(
        "SELECT amname::text
           FROM pg_catalog.pg_am
          WHERE amname = 'pgcontext_hnsw'",
        "installed access method query should succeed",
    )
}

fn installed_aggregates() -> std::collections::BTreeSet<(String, String)> {
    names_from_query(
        "SELECT proname::text
                || '('
                || pg_catalog.oidvectortypes(proargtypes)::text
                || ')'
           FROM pg_catalog.pg_proc
           JOIN pg_catalog.pg_namespace
             ON pg_namespace.oid = pg_proc.pronamespace
          WHERE pg_namespace.nspname = 'pgcontext'
            AND prokind = 'a'
          ORDER BY 1",
        "installed aggregate query should succeed",
    )
}

fn installed_casts() -> std::collections::BTreeSet<(String, String)> {
    names_from_query(
        "SELECT pg_catalog.format_type(castsource, NULL)::text
                || ' AS '
                || pg_catalog.format_type(casttarget, NULL)::text
           FROM pg_catalog.pg_cast
          WHERE castsource IN (
                    'vector'::regtype,
                    'halfvec'::regtype,
                    'sparsevec'::regtype,
                    'bitvec'::regtype
                )
             OR casttarget IN (
                    'vector'::regtype,
                    'halfvec'::regtype,
                    'sparsevec'::regtype,
                    'bitvec'::regtype
                )
          ORDER BY 1",
        "installed cast query should succeed",
    )
}

fn installed_contract_types() -> std::collections::BTreeSet<(String, String)> {
    names_from_query(
        "SELECT typname::text
           FROM pg_catalog.pg_type
           JOIN pg_catalog.pg_namespace
             ON pg_namespace.oid = pg_type.typnamespace
          WHERE typname NOT LIKE '\\_%'
            AND (
                    typname IN (
                        'vector', 'halfvec', 'sparsevec', 'bitvec',
                        'buildjobstatus', 'embeddingmigrationstatus',
                        'indexadvisorrecommendation', 'indexdiagnosticstatus',
                        'indexlifecyclestatus', 'indexmemoryestimatestatus',
                        'optimizationstatus', 'querycohortstatus',
                        'queryexplainstatus', 'querylatencybucket',
                        'querylifecyclestate', 'recallcheckstatus',
                        'telemetrystatus', 'vacuumadvicestatus'
                    )
                 OR pg_namespace.nspname = 'pgcontext'
                )
          ORDER BY 1",
        "installed type query should succeed",
    )
}

fn installed_operators() -> std::collections::BTreeSet<(String, String)> {
    objects_from_query(
        "SELECT oprname::text,
                pg_catalog.format_type(oprleft, NULL)::text
                || ', '
                || pg_catalog.format_type(oprright, NULL)::text
           FROM pg_catalog.pg_operator
           JOIN pg_catalog.pg_namespace
             ON pg_namespace.oid = pg_operator.oprnamespace
          WHERE pg_namespace.nspname = 'pgcontext'
          ORDER BY 1",
        "installed operator query should succeed",
    )
}

fn installed_operator_classes() -> std::collections::BTreeSet<(String, String)> {
    objects_from_query(
        "SELECT opcname::text,
                pg_am.amname::text
                || ', '
                || pg_catalog.format_type(opcintype, NULL)::text
           FROM pg_catalog.pg_opclass
           JOIN pg_catalog.pg_namespace
             ON pg_namespace.oid = pg_opclass.opcnamespace
           JOIN pg_catalog.pg_am
             ON pg_am.oid = pg_opclass.opcmethod
          WHERE pg_namespace.nspname = 'pgcontext'
          ORDER BY 1",
        "installed operator class query should succeed",
    )
}

fn installed_schemas() -> std::collections::BTreeSet<(String, String)> {
    names_from_query(
        "SELECT nspname::text
           FROM pg_catalog.pg_namespace
          WHERE nspname = 'pgcontext'",
        "installed schema query should succeed",
    )
}

fn installed_triggers() -> std::collections::BTreeSet<(String, String)> {
    names_from_query(
        "SELECT pg_class.relname::text || '.' || tgname::text
           FROM pg_catalog.pg_trigger
           JOIN pg_catalog.pg_class
             ON pg_class.oid = pg_trigger.tgrelid
           JOIN pg_catalog.pg_namespace
             ON pg_namespace.oid = pg_class.relnamespace
          WHERE pg_namespace.nspname = 'pgcontext'
            AND NOT tgisinternal
          ORDER BY 1",
        "installed trigger query should succeed",
    )
}

fn installed_relations(relkind: &str) -> std::collections::BTreeSet<(String, String)> {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT relname::text
               FROM pg_catalog.pg_class
               JOIN pg_catalog.pg_namespace
                 ON pg_namespace.oid = pg_class.relnamespace
              WHERE pg_namespace.nspname = 'pgcontext'
                AND relkind::text = $1
              ORDER BY 1",
            None,
            &[relkind.into()],
        )?;

        let mut names = std::collections::BTreeSet::new();
        for row in rows {
            names.insert((
                row.get::<String>(1)?.expect("name should not be null"),
                String::new(),
            ));
        }
        Ok::<_, spi::Error>(names)
    })
    .expect("installed relation query should succeed")
}

fn names_from_query(
    sql: &str,
    error_message: &'static str,
) -> std::collections::BTreeSet<(String, String)> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut names = std::collections::BTreeSet::new();
        for row in rows {
            names.insert((
                row.get::<String>(1)?.expect("name should not be null"),
                String::new(),
            ));
        }
        Ok::<_, spi::Error>(names)
    })
    .expect(error_message)
}

fn objects_from_query(
    sql: &str,
    error_message: &'static str,
) -> std::collections::BTreeSet<(String, String)> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut objects = std::collections::BTreeSet::new();
        for row in rows {
            objects.insert((
                row.get::<String>(1)?.expect("name should not be null"),
                row.get::<String>(2)?.expect("identity should not be null"),
            ));
        }
        Ok::<_, spi::Error>(objects)
    })
    .expect(error_message)
}
