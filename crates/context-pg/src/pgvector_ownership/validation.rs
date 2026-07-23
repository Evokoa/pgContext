use pgrx::prelude::*;

use crate::error::raise_sql_error;

#[derive(Debug, Clone)]
pub(super) struct ConversionTarget {
    pub(super) table_oid: pg_sys::Oid,
    pub(super) schema_name: String,
    pub(super) table_name: String,
    pub(super) owner_role: pg_sys::Oid,
    pub(super) attnum: i16,
    pub(super) column_name: String,
    pub(super) source_type_oid: pg_sys::Oid,
    pub(super) source_type_name: String,
    pub(super) source_typmod: i32,
    pub(super) not_null: bool,
    pub(super) dimensions: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DependencyInventory {
    pub(super) blockers: Vec<String>,
    pub(super) manifest: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) struct IndexPlan {
    pub(super) index_name: String,
    pub(super) canonical_opclass: &'static str,
    pub(super) options: Vec<String>,
    pub(super) tablespace: Option<String>,
}

pub(super) fn ensure_certified_bridge() {
    let certified = Spi::get_one::<bool>(
        "WITH certified_extensions AS (
             SELECT bridge.oid AS bridge_oid,
                    pgcontext.oid AS pgcontext_oid,
                    pgvector.oid AS pgvector_oid
               FROM pg_catalog.pg_extension AS bridge
               JOIN pg_catalog.pg_extension AS pgcontext
                 ON pgcontext.extname = 'pgcontext'
               JOIN pg_catalog.pg_extension AS pgvector
                 ON pgvector.extname = 'vector'
               JOIN pg_catalog.pg_namespace AS pgvector_namespace
                 ON pgvector_namespace.oid = pgvector.extnamespace
              WHERE bridge.extname = 'pgcontext_pgvector'
                AND bridge.extversion = '0.2.0'
                AND pgcontext.extversion = '0.2.0'
                AND pgvector.extversion ~ '^0[.]8[.][0-9]+$'
                AND pgvector_namespace.nspname = 'public'
         )
         SELECT EXISTS (
             SELECT 1
               FROM certified_extensions AS extensions
              WHERE (
                    SELECT count(*)
                      FROM pg_catalog.pg_type AS source_type
                      JOIN pg_catalog.pg_namespace AS source_namespace
                        ON source_namespace.oid = source_type.typnamespace
                      JOIN pg_catalog.pg_type AS target_type
                        ON target_type.typname = source_type.typname
                      JOIN pg_catalog.pg_namespace AS target_namespace
                        ON target_namespace.oid = target_type.typnamespace
                     WHERE source_namespace.nspname = 'public'
                       AND target_namespace.nspname = 'pgcontext'
                       AND source_type.typname IN ('vector', 'halfvec', 'sparsevec')
                       AND source_type.typlen = target_type.typlen
                       AND source_type.typbyval = target_type.typbyval
                       AND source_type.typalign = target_type.typalign
                       AND source_type.typstorage = 'e'
                       AND target_type.typstorage = 'x'
                       AND EXISTS (
                           SELECT 1 FROM pg_catalog.pg_depend AS dependency
                            WHERE dependency.classid = 'pg_catalog.pg_type'::pg_catalog.regclass
                              AND dependency.objid = source_type.oid
                              AND dependency.refobjid = extensions.pgvector_oid
                              AND dependency.deptype = 'e'
                       )
                       AND EXISTS (
                           SELECT 1 FROM pg_catalog.pg_depend AS dependency
                            WHERE dependency.classid = 'pg_catalog.pg_type'::pg_catalog.regclass
                              AND dependency.objid = target_type.oid
                              AND dependency.refobjid = extensions.pgcontext_oid
                              AND dependency.deptype = 'e'
                       )
                   ) = 3
                AND (
                    SELECT count(*)
                      FROM pg_catalog.pg_cast AS cast_entry
                      JOIN pg_catalog.pg_depend AS dependency
                        ON dependency.classid = 'pg_catalog.pg_cast'::pg_catalog.regclass
                       AND dependency.objid = cast_entry.oid
                       AND dependency.refobjid = extensions.bridge_oid
                       AND dependency.deptype = 'e'
                     WHERE cast_entry.castsource IN (
                               pg_catalog.to_regtype('public.vector'),
                               pg_catalog.to_regtype('public.halfvec')
                           )
                       AND cast_entry.casttarget IN (
                               pg_catalog.to_regtype('pgcontext.vector'),
                               pg_catalog.to_regtype('pgcontext.halfvec')
                           )
                       AND cast_entry.castmethod = 'b'
                       AND cast_entry.castcontext = 'a'
                   ) = 2
                AND (
                    SELECT count(*)
                      FROM pg_catalog.pg_cast AS cast_entry
                      JOIN pg_catalog.pg_depend AS dependency
                        ON dependency.classid = 'pg_catalog.pg_cast'::pg_catalog.regclass
                       AND dependency.objid = cast_entry.oid
                       AND dependency.refobjid = extensions.bridge_oid
                       AND dependency.deptype = 'e'
                     WHERE (
                               cast_entry.castsource = pg_catalog.to_regtype('public.sparsevec')
                           AND cast_entry.casttarget = pg_catalog.to_regtype('pgcontext.sparsevec')
                           OR  cast_entry.castsource = pg_catalog.to_regtype('pgcontext.sparsevec')
                           AND cast_entry.casttarget = pg_catalog.to_regtype('public.sparsevec')
                           )
                       AND cast_entry.castmethod = 'f'
                       AND cast_entry.castcontext = 'a'
                   ) = 2
                AND (
                    SELECT count(*) = 12 AND bool_and(pg_catalog.amvalidate(opclass.oid))
                      FROM pg_catalog.pg_opclass AS opclass
                      JOIN pg_catalog.pg_depend AS dependency
                        ON dependency.classid = 'pg_catalog.pg_opclass'::pg_catalog.regclass
                       AND dependency.objid = opclass.oid
                       AND dependency.refobjid = extensions.bridge_oid
                       AND dependency.deptype = 'e'
                   )
                AND (
                    SELECT count(*)
                      FROM pg_catalog.pg_proc AS procedure
                      JOIN pg_catalog.pg_depend AS dependency
                        ON dependency.classid = 'pg_catalog.pg_proc'::pg_catalog.regclass
                       AND dependency.objid = procedure.oid
                       AND dependency.refobjid = extensions.bridge_oid
                       AND dependency.deptype = 'e'
                     WHERE procedure.proname LIKE '_pgvector_%_support'
                   ) = 12
         )",
    )
    .unwrap_or(Some(false))
    .unwrap_or(false);
    if !certified {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
            "pgvector ownership conversion requires the certified \
             pgcontext_pgvector 0.2.0 bridge with pgcontext 0.2.0 and \
             pgvector 0.8.x",
        );
    }
}

pub(super) fn resolve_conversion_target(
    table_oid: pg_sys::Oid,
    column_name: &str,
) -> ConversionTarget {
    let target = Spi::connect(|client| {
        let row = client
            .select(
                "SELECT namespace.nspname::text,
                        relation.relname::text,
                        relation.relowner,
                        relation.relkind::text,
                        relation.relpersistence::text,
                        attribute.attnum::int4,
                        type.oid,
                        type.typname::text,
                        attribute.atttypmod,
                        attribute.attnotnull,
                        pg_catalog.pg_has_role(SESSION_USER, relation.relowner, 'MEMBER'),
                        EXISTS (
                            SELECT 1
                              FROM pg_catalog.pg_depend AS dependency
                              JOIN pg_catalog.pg_extension AS extension
                                ON extension.oid = dependency.refobjid
                             WHERE dependency.classid = 'pg_catalog.pg_type'::pg_catalog.regclass
                               AND dependency.objid = type.oid
                               AND dependency.deptype = 'e'
                               AND extension.extname = 'vector'
                        )
                   FROM pg_catalog.pg_class AS relation
                   JOIN pg_catalog.pg_namespace AS namespace
                     ON namespace.oid = relation.relnamespace
                   JOIN pg_catalog.pg_attribute AS attribute
                     ON attribute.attrelid = relation.oid
                   JOIN pg_catalog.pg_type AS type
                     ON type.oid = attribute.atttypid
                  WHERE relation.oid = $1
                    AND attribute.attname = $2
                    AND attribute.attnum > 0
                    AND NOT attribute.attisdropped",
                None,
                &[table_oid.into(), column_name.into()],
            )?
            .first();
        if row.is_empty() {
            return Ok(None);
        }
        Ok::<_, spi::Error>(Some((
            row.get::<String>(1)?.unwrap_or_default(),
            row.get::<String>(2)?.unwrap_or_default(),
            row.get::<pg_sys::Oid>(3)?.unwrap_or(pg_sys::InvalidOid),
            row.get::<String>(4)?.unwrap_or_default(),
            row.get::<String>(5)?.unwrap_or_default(),
            row.get::<i32>(6)?.unwrap_or_default(),
            row.get::<pg_sys::Oid>(7)?.unwrap_or(pg_sys::InvalidOid),
            row.get::<String>(8)?.unwrap_or_default(),
            row.get::<i32>(9)?.unwrap_or(-1),
            row.get::<bool>(10)?.unwrap_or(false),
            row.get::<bool>(11)?.unwrap_or(false),
            row.get::<bool>(12)?.unwrap_or(false),
        )))
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to resolve pgvector conversion target: {error}"),
        )
    });

    let Some((
        schema_name,
        table_name,
        owner_role,
        relkind,
        persistence,
        attnum,
        source_type_oid,
        source_type_name,
        source_typmod,
        not_null,
        caller_is_owner,
        pgvector_owned,
    )) = target
    else {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
            format!("conversion target column does not exist: {column_name}"),
        );
    };

    if !caller_is_owner {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            format!("must own conversion target {schema_name}.{table_name}"),
        );
    }
    if relkind != "r" || persistence != "p" {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
            "ownership conversion supports permanent ordinary heap tables only",
        );
    }
    if !pgvector_owned
        || !matches!(
            source_type_name.as_str(),
            "vector" | "halfvec" | "sparsevec"
        )
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATATYPE_MISMATCH,
            format!(
                "conversion target must be a directly pgvector-owned vector, halfvec, or sparsevec column, found {source_type_name}"
            ),
        );
    }
    if source_type_name == "sparsevec"
        && source_typmod
            > i32::try_from(context_core::policy::MAX_VECTOR_DIMENSIONS).unwrap_or(i32::MAX)
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!(
                "pgvector sparsevec dimensions {source_typmod} exceed pgContext's current limit {}; \
                 large-dimension sparse support is planned",
                context_core::policy::MAX_VECTOR_DIMENSIONS
            ),
        );
    }
    let attnum = i16::try_from(attnum).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "conversion target attribute number exceeds int2",
        )
    });

    ConversionTarget {
        table_oid,
        schema_name,
        table_name,
        owner_role,
        attnum,
        column_name: column_name.to_owned(),
        source_type_oid,
        source_type_name,
        source_typmod,
        not_null,
        dimensions: (source_typmod > 0).then_some(source_typmod),
    }
}

pub(super) fn target_type_sql(target: &ConversionTarget) -> String {
    match target.dimensions {
        Some(dimensions) => format!("pgcontext.{}({dimensions})", target.source_type_name),
        None => format!("pgcontext.{}", target.source_type_name),
    }
}

pub(super) fn canonical_opclass(type_name: &str, metric: &str) -> Option<&'static str> {
    match (type_name, metric) {
        ("vector", "l2") => Some("pgcontext.vector_hnsw_ops"),
        ("vector", "inner_product") => Some("pgcontext.vector_hnsw_ip_ops"),
        ("vector", "cosine") => Some("pgcontext.vector_hnsw_cosine_ops"),
        ("vector", "l1") => Some("pgcontext.vector_hnsw_l1_ops"),
        ("halfvec", "l2") => Some("pgcontext.halfvec_hnsw_ops"),
        ("halfvec", "inner_product") => Some("pgcontext.halfvec_hnsw_ip_ops"),
        ("halfvec", "cosine") => Some("pgcontext.halfvec_hnsw_cosine_ops"),
        ("halfvec", "l1") => Some("pgcontext.halfvec_hnsw_l1_ops"),
        ("sparsevec", "l2") => Some("pgcontext.sparsevec_hnsw_ops"),
        ("sparsevec", "inner_product") => Some("pgcontext.sparsevec_hnsw_ip_ops"),
        ("sparsevec", "cosine") => Some("pgcontext.sparsevec_hnsw_cosine_ops"),
        ("sparsevec", "l1") => Some("pgcontext.sparsevec_hnsw_l1_ops"),
        _ => None,
    }
}

fn source_metric(extension: &str, opclass: &str) -> Option<&'static str> {
    match (extension, opclass) {
        ("vector", "vector_l2_ops" | "halfvec_l2_ops" | "sparsevec_l2_ops")
        | (
            "pgcontext_pgvector",
            "vector_hnsw_pgvector_l2_ops"
            | "halfvec_hnsw_pgvector_l2_ops"
            | "sparsevec_hnsw_pgvector_l2_ops",
        ) => Some("l2"),
        ("vector", "vector_ip_ops" | "halfvec_ip_ops" | "sparsevec_ip_ops")
        | (
            "pgcontext_pgvector",
            "vector_hnsw_pgvector_ip_ops"
            | "halfvec_hnsw_pgvector_ip_ops"
            | "sparsevec_hnsw_pgvector_ip_ops",
        ) => Some("inner_product"),
        ("vector", "vector_cosine_ops" | "halfvec_cosine_ops" | "sparsevec_cosine_ops")
        | (
            "pgcontext_pgvector",
            "vector_hnsw_pgvector_cosine_ops"
            | "halfvec_hnsw_pgvector_cosine_ops"
            | "sparsevec_hnsw_pgvector_cosine_ops",
        ) => Some("cosine"),
        ("vector", "vector_l1_ops" | "halfvec_l1_ops" | "sparsevec_l1_ops")
        | (
            "pgcontext_pgvector",
            "vector_hnsw_pgvector_l1_ops"
            | "halfvec_hnsw_pgvector_l1_ops"
            | "sparsevec_hnsw_pgvector_l1_ops",
        ) => Some("l1"),
        _ => None,
    }
}

pub(super) fn dependency_inventory(
    target: &ConversionTarget,
    ignored_trigger: Option<&str>,
    check_prepared_statements: bool,
) -> DependencyInventory {
    let ignored_trigger = ignored_trigger.unwrap_or("");
    let rows = Spi::connect(|client| {
        let rows = client.select(
            r#"
WITH target AS (
    SELECT relation.oid AS table_oid,
           relation.reltype,
           relation.relrowsecurity,
           relation.relforcerowsecurity,
           attribute.attnum,
           attribute.attgenerated,
           attribute.atthasdef,
           attribute.attacl,
           attribute.attnotnull,
           attribute.attstattarget,
           attribute.attoptions,
           attribute.attstorage,
           attribute.attcompression,
           type.typstorage
      FROM pg_catalog.pg_class AS relation
      JOIN pg_catalog.pg_attribute AS attribute
        ON attribute.attrelid = relation.oid
      JOIN pg_catalog.pg_type AS type
        ON type.oid = attribute.atttypid
     WHERE relation.oid = $1
       AND attribute.attnum = $2
), findings AS (
    SELECT 'blocker'::text AS kind, 'column defaults are not supported'::text AS detail
      FROM target WHERE atthasdef
    UNION ALL
    SELECT 'blocker', 'generated columns are not supported'
      FROM target WHERE attgenerated <> ''
    UNION ALL
    SELECT 'blocker', 'dependent generated columns or defaults are not supported'
      FROM target
     WHERE EXISTS (
         SELECT 1
           FROM pg_catalog.pg_depend AS dependency
          WHERE dependency.classid = 'pg_catalog.pg_attrdef'::pg_catalog.regclass
            AND dependency.refclassid = 'pg_catalog.pg_class'::pg_catalog.regclass
            AND dependency.refobjid = table_oid
            AND dependency.refobjsubid = attnum
     )
    UNION ALL
    SELECT 'blocker', 'column-level ACLs are not supported'
      FROM target WHERE attacl IS NOT NULL
    UNION ALL
    SELECT 'blocker', 'column comments are not supported'
      FROM target
     WHERE EXISTS (
         SELECT 1 FROM pg_catalog.pg_description
          WHERE classoid = 'pg_catalog.pg_class'::pg_catalog.regclass
            AND objoid = table_oid
            AND objsubid = attnum
     )
    UNION ALL
    SELECT 'blocker', 'nondefault column statistics or options are not supported'
      FROM target WHERE attstattarget <> -1 OR attoptions IS NOT NULL
    UNION ALL
    SELECT 'blocker', 'nondefault column storage or compression is not supported'
      FROM target
     WHERE attstorage <> typstorage OR attcompression::text <> ''
    UNION ALL
    SELECT 'blocker', 'row-level security enabled on the table is not supported'
      FROM target WHERE relrowsecurity OR relforcerowsecurity
    UNION ALL
    SELECT 'blocker', 'partitioned or inherited tables are not supported'
      FROM target
     WHERE EXISTS (SELECT 1 FROM pg_catalog.pg_inherits WHERE inhrelid = table_oid OR inhparent = table_oid)
    UNION ALL
    SELECT 'blocker', 'dependent views, materialized views, or rules are not supported'
      FROM target
     WHERE EXISTS (
         SELECT 1
           FROM pg_catalog.pg_depend AS dependency
          WHERE dependency.classid = 'pg_catalog.pg_rewrite'::pg_catalog.regclass
            AND dependency.refclassid = 'pg_catalog.pg_class'::pg_catalog.regclass
            AND dependency.refobjid = table_oid
            AND dependency.refobjsubid = attnum
     )
    UNION ALL
    SELECT 'blocker', 'dependent stored functions are not supported'
      FROM target
     WHERE EXISTS (
         SELECT 1
           FROM pg_catalog.pg_depend AS dependency
          WHERE dependency.classid = 'pg_catalog.pg_proc'::pg_catalog.regclass
            AND dependency.refclassid = 'pg_catalog.pg_class'::pg_catalog.regclass
            AND dependency.refobjid = table_oid
            AND dependency.refobjsubid = attnum
     )
    UNION ALL
    SELECT 'blocker', 'constraints on the conversion column are not supported'
      FROM target
     WHERE EXISTS (
         SELECT 1 FROM pg_catalog.pg_constraint
          WHERE conrelid = table_oid AND attnum = ANY (conkey)
     )
    UNION ALL
    SELECT 'blocker', 'row-level security policies are not supported'
      FROM target
     WHERE EXISTS (SELECT 1 FROM pg_catalog.pg_policy WHERE polrelid = table_oid)
    UNION ALL
    SELECT 'blocker', 'user triggers are not supported'
      FROM target
     WHERE EXISTS (
         SELECT 1 FROM pg_catalog.pg_trigger
          WHERE tgrelid = table_oid
            AND NOT tgisinternal
            AND tgname <> $3
     )
    UNION ALL
    SELECT 'blocker', 'logical-replication publications are not supported'
      FROM target
     WHERE EXISTS (
         SELECT 1
           FROM pg_catalog.pg_publication AS publication
          WHERE publication.puballtables
     ) OR EXISTS (
         SELECT 1 FROM pg_catalog.pg_publication_rel WHERE prrelid = table_oid
     )
    UNION ALL
    SELECT 'blocker', 'extended statistics on the conversion column are not supported'
      FROM target
     WHERE EXISTS (
         SELECT 1 FROM pg_catalog.pg_statistic_ext
          WHERE stxrelid = table_oid AND attnum = ANY (stxkeys)
     )
    UNION ALL
    SELECT 'blocker', 'replica identity indexes on the conversion column are not supported'
      FROM target
     WHERE EXISTS (
         SELECT 1 FROM pg_catalog.pg_index
          WHERE indrelid = table_oid
            AND indisreplident
            AND attnum = ANY (indkey)
     )
    UNION ALL
    SELECT 'blocker', 'expression, partial, multicolumn, INCLUDE, or unsupported indexes are not supported'
      FROM target
     WHERE EXISTS (
         SELECT 1
           FROM pg_catalog.pg_index AS index
           JOIN pg_catalog.pg_class AS index_relation ON index_relation.oid = index.indexrelid
           JOIN pg_catalog.pg_am AS access_method ON access_method.oid = index_relation.relam
          WHERE index.indrelid = table_oid
            AND (
                attnum = ANY (index.indkey)
                OR EXISTS (
                    SELECT 1 FROM pg_catalog.pg_depend AS dependency
                     WHERE dependency.classid = 'pg_catalog.pg_class'::pg_catalog.regclass
                       AND dependency.objid = index.indexrelid
                       AND dependency.refclassid = 'pg_catalog.pg_class'::pg_catalog.regclass
                       AND dependency.refobjid = table_oid
                       AND dependency.refobjsubid = attnum
                )
            )
            AND (
                index.indexprs IS NOT NULL
                OR index.indpred IS NOT NULL
                OR index.indnkeyatts <> 1
                OR index.indnatts <> 1
                OR index.indkey[0] <> attnum
                OR access_method.amname NOT IN ('hnsw', 'ivfflat', 'pgcontext_hnsw')
            )
     )
    UNION ALL
    SELECT 'blocker', 'dependent composite row types are not supported'
      FROM target
     WHERE EXISTS (
         SELECT 1
           FROM pg_catalog.pg_depend AS dependency
          WHERE dependency.refclassid = 'pg_catalog.pg_type'::pg_catalog.regclass
            AND dependency.refobjid = reltype
            AND NOT (
                dependency.classid = 'pg_catalog.pg_type'::pg_catalog.regclass
                AND dependency.deptype = 'i'
            )
     )
    UNION ALL
    SELECT 'manifest', pg_catalog.format(
        'target:%s:%s:%s:%s:%s:%s',
        relation.oid,
        attribute.attnum,
        attribute.atttypid,
        attribute.atttypmod,
        attribute.attname,
        attribute.attnotnull
    )
      FROM pg_catalog.pg_class AS relation
      JOIN pg_catalog.pg_attribute AS attribute ON attribute.attrelid = relation.oid
     WHERE relation.oid = $1 AND attribute.attnum = $2
    UNION ALL
    SELECT 'manifest', pg_catalog.format(
        'index:%s:%s:%s:%s:%s:%s:%s:%s',
        index.indexrelid,
        index.indkey,
        index.indclass,
        index_relation.relam,
        coalesce(index_relation.reloptions::text, '{}'),
        coalesce(index.indexprs::text, ''),
        coalesce(index.indpred::text, ''),
        index_relation.reltablespace
    )
      FROM target
      JOIN pg_catalog.pg_index AS index ON index.indrelid = target.table_oid
      JOIN pg_catalog.pg_class AS index_relation ON index_relation.oid = index.indexrelid
     WHERE target.attnum = ANY (index.indkey)
)
SELECT kind, detail FROM findings ORDER BY kind, detail
"#,
            None,
            &[
                target.table_oid.into(),
                i32::from(target.attnum).into(),
                ignored_trigger.into(),
            ],
        )?;
        rows.map(|row| {
            Ok::<_, spi::Error>((
                row.get::<String>(1)?.unwrap_or_default(),
                row.get::<String>(2)?.unwrap_or_default(),
            ))
        })
        .collect::<Result<Vec<_>, _>>()
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to inventory conversion dependencies: {error}"),
        )
    });

    let mut blockers = Vec::new();
    let mut manifest = Vec::new();
    for (kind, detail) in rows {
        if kind == "blocker" {
            blockers.push(detail);
        } else if kind == "manifest" {
            manifest.push(detail);
        }
    }
    if check_prepared_statements {
        let prepared_statements =
            Spi::get_one::<i64>("SELECT count(*)::bigint FROM pg_catalog.pg_prepared_statements")
                .unwrap_or(Some(0))
                .unwrap_or(0);
        if prepared_statements > 0 {
            blockers.push(
                "current session has prepared statements; DEALLOCATE ALL before conversion"
                    .to_owned(),
            );
        }
    }
    blockers.sort();
    blockers.dedup();
    manifest.sort();
    manifest.dedup();
    DependencyInventory { blockers, manifest }
}

pub(super) fn ensure_no_blockers(inventory: &DependencyInventory) {
    if !inventory.blockers.is_empty() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
            format!(
                "pgvector ownership conversion is blocked: {}",
                inventory.blockers.join("; ")
            ),
        );
    }
}

pub(super) fn collect_fast_index_plans(target: &ConversionTarget) -> Vec<IndexPlan> {
    let rows = Spi::connect(|client| {
        let rows = client.select(
            "SELECT index_relation.relname::text,
                    opclass.opcname::text,
                    COALESCE(index_relation.reloptions, '{}')::text[],
                    NULLIF(tablespace.spcname, 'pg_default')::text,
                    extension.extname::text,
                    access_method.amname::text,
                    index.indisvalid AND index.indisready AND index.indislive,
                    pg_catalog.obj_description(index_relation.oid, 'pg_class') IS NOT NULL
               FROM pg_catalog.pg_index AS index
               JOIN pg_catalog.pg_class AS index_relation
                 ON index_relation.oid = index.indexrelid
               JOIN pg_catalog.pg_opclass AS opclass
                 ON opclass.oid = index.indclass[0]
               JOIN pg_catalog.pg_am AS access_method
                 ON access_method.oid = index_relation.relam
               LEFT JOIN pg_catalog.pg_depend AS dependency
                 ON dependency.classid = 'pg_catalog.pg_opclass'::pg_catalog.regclass
                AND dependency.objid = opclass.oid
                AND dependency.deptype = 'e'
               LEFT JOIN pg_catalog.pg_extension AS extension
                 ON extension.oid = dependency.refobjid
               LEFT JOIN pg_catalog.pg_tablespace AS tablespace
                 ON tablespace.oid = index_relation.reltablespace
              WHERE index.indrelid = $1
                AND index.indkey[0] = $2
              ORDER BY index_relation.relname",
            None,
            &[target.table_oid.into(), i32::from(target.attnum).into()],
        )?;
        rows.map(|row| {
            Ok::<_, spi::Error>((
                row.get::<String>(1)?.unwrap_or_default(),
                row.get::<String>(2)?.unwrap_or_default(),
                row.get::<Vec<String>>(3)?.unwrap_or_default(),
                row.get::<String>(4)?,
                row.get::<String>(5)?.unwrap_or_default(),
                row.get::<String>(6)?.unwrap_or_default(),
                row.get::<bool>(7)?.unwrap_or(false),
                row.get::<bool>(8)?.unwrap_or(false),
            ))
        })
        .collect::<Result<Vec<_>, _>>()
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to inventory conversion indexes: {error}"),
        )
    });

    rows.into_iter()
        .map(
            |(
                index_name,
                opclass,
                options,
                tablespace,
                extension,
                access_method,
                healthy,
                has_comment,
            )| {
                if !healthy {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
                        format!(
                            "source index {index_name} is not valid, ready, and live; its state cannot be preserved"
                        ),
                    );
                }
                if has_comment {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
                        format!(
                            "source index {index_name} has a comment that ownership conversion cannot preserve"
                        ),
                    );
                }
            let Some(metric) = source_metric(&extension, &opclass) else {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
                    format!(
                        "no certified pgContext equivalent for {extension}-owned opclass {opclass}"
                    ),
                );
            };
            let canonical_opclass = canonical_opclass(&target.source_type_name, metric)
                .unwrap_or_else(|| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
                        format!(
                            "no canonical pgContext opclass for {} metric {metric}",
                            target.source_type_name
                        ),
                    )
                });
            if access_method != "ivfflat" && !options.is_empty() {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
                    format!(
                        "source index {index_name} has per-index options that pgcontext_hnsw cannot preserve"
                    ),
                );
            }
            IndexPlan {
                index_name,
                canonical_opclass,
                options: Vec::new(),
                tablespace,
            }
            },
        )
        .collect()
}

pub(super) fn ensure_index_build_privileges(target: &ConversionTarget, plans: &[IndexPlan]) {
    let schema_create = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.has_schema_privilege(SESSION_USER, namespace.oid, 'CREATE')
           FROM pg_catalog.pg_namespace AS namespace
          WHERE namespace.nspname = $1",
        &[target.schema_name.clone().into()],
    )
    .unwrap_or(Some(false))
    .unwrap_or(false);
    if !schema_create {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            format!(
                "ownership conversion requires CREATE on schema {} for caller-executed index builds",
                target.schema_name
            ),
        );
    }
    for tablespace in plans.iter().filter_map(|plan| plan.tablespace.as_deref()) {
        let can_create = Spi::get_one_with_args::<bool>(
            "SELECT pg_catalog.has_tablespace_privilege(SESSION_USER, $1, 'CREATE')",
            &[tablespace.into()],
        )
        .unwrap_or(Some(false))
        .unwrap_or(false);
        if !can_create {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
                format!(
                    "ownership conversion requires CREATE on tablespace {tablespace} to preserve source index placement"
                ),
            );
        }
    }
}

pub(super) fn ensure_online_index_profile(target: &ConversionTarget, requested_metric: &str) {
    let plans = collect_fast_index_plans(target);
    if plans.len() > 1 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
            "restricted-online conversion supports at most one source ANN index",
        );
    }
    if let Some(plan) = plans.first() {
        let expected =
            canonical_opclass(&target.source_type_name, requested_metric).unwrap_or_default();
        if plan.canonical_opclass != expected {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
                format!(
                    "restricted-online metric {requested_metric} does not match source index {}",
                    plan.index_name
                ),
            );
        }
    }
}
