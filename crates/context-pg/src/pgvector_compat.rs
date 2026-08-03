//! pgvector migration inventory, index-adoption planning, comparison tools,
//! and the once-per-backend bridge advisory.
//!
//! The main extension owns canonical vector types in `pgcontext` and has no
//! dependency on pgvector until the extension owner explicitly enables the
//! conflict-safe binding. Optional binding and facade objects are owned by the
//! exact main-extension owner but remain outside extension membership so
//! `pg_dump` emits their DDL. Inventory remains useful without them because it
//! discovers objects by extension ownership rather than unqualified type names.

#![allow(
    unsafe_code,
    reason = "syscache extension-ownership lookups and relcache field reads \
              are isolated here behind safe, nudge-only entry points"
)]

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::ffi::CStr;

use pgrx::PgRelation;
use pgrx::PgTryBuilder;
use pgrx::prelude::*;

use crate::error::raise_sql_error;

thread_local! {
    static PGVECTOR_NUDGED_INDEXES: RefCell<BTreeSet<u32>> =
        const { RefCell::new(BTreeSet::new()) };
}

type MigrationReportRow = (
    String,
    String,
    String,
    String,
    Option<i32>,
    Vec<String>,
    Vec<String>,
    bool,
    Vec<String>,
    String,
);

const COMPATIBILITY_INVENTORY: &[(&str, &str, &str, &str, &str)] = &[
    (
        "type",
        "vector, halfvec",
        "compatible",
        "pgcontext.vector, pgcontext.halfvec",
        "dense values have a certified lossless ownership conversion",
    ),
    (
        "type",
        "sparsevec",
        "translated",
        "pgcontext.sparsevec",
        "validated value rewrite; coordinates above the pgContext limit fail explicitly",
    ),
    (
        "type",
        "bit",
        "compatible",
        "pgcontext.bitvec or PostgreSQL bit",
        "bit uses PostgreSQL ownership or explicit bitvec; expression indexes are preflighted separately",
    ),
    (
        "operator",
        "<->, <#>, <=>, <+>, <~>, <%>",
        "compatible",
        "same operator spellings in the owning type schema",
        "operator OIDs remain type-owner-specific; search_path never changes ownership",
    ),
    (
        "function",
        "distance, dimensions, norm, aggregate, casts",
        "compatible",
        "schema-qualified pgContext equivalents",
        "prepared statements must be drained across a type-OID cutover",
    ),
    (
        "function",
        "subvector, binary_quantize, and vector arithmetic",
        "unsupported",
        "retain pgvector expression until explicitly rewritten",
        "migration preflight reports expression dependencies instead of guessing",
    ),
    (
        "access_method",
        "hnsw",
        "translated",
        "pgcontext_hnsw",
        "never claims binary index-page compatibility",
    ),
    (
        "access_method",
        "ivfflat",
        "translated",
        "pgcontext_ivfflat",
        "lists is preserved; source rows stay authoritative during rebuild and rollback",
    ),
    (
        "setting",
        "ivfflat.probes",
        "translated",
        "pgcontext.ivfflat_probes",
        "same initial-list meaning; pgContext work budgets remain independently bounded",
    ),
    (
        "setting",
        "hnsw.ef_search",
        "translated",
        "pgcontext.hnsw_ef_search",
        "candidate-search width only; no ignored alias",
    ),
    (
        "setting",
        "hnsw.iterative_scan",
        "unsupported",
        "use pgContext filtered-search budgets",
        "pgContext does not silently accept pgvector ordering modes it cannot preserve",
    ),
    (
        "setting",
        "ivfflat.iterative_scan",
        "translated",
        "pgcontext.ivfflat_iterative_scan",
        "off, strict_order, and relaxed_order are preserved",
    ),
];

/// Published compatibility matrix for pgvector SQL and migration surfaces.
#[pg_extern]
#[search_path(pg_catalog, pgcontext)]
fn pgvector_compatibility_inventory() -> TableIterator<
    'static,
    (
        name!(category, String),
        name!(pgvector_surface, String),
        name!(status, String),
        name!(pgcontext_surface, String),
        name!(notes, String),
    ),
> {
    TableIterator::new(COMPATIBILITY_INVENTORY.iter().map(
        |(category, pgvector_surface, status, pgcontext_surface, notes)| {
            (
                (*category).to_owned(),
                (*pgvector_surface).to_owned(),
                (*status).to_owned(),
                (*pgcontext_surface).to_owned(),
                (*notes).to_owned(),
            )
        },
    ))
}

/// Returns whether `type_oid` belongs to the pgvector extension.
pub(crate) fn type_owned_by_pgvector(type_oid: pg_sys::Oid) -> bool {
    // SAFETY: syscache lookups over a valid type OID; both functions accept
    // arbitrary OIDs and return InvalidOid/NULL for misses.
    unsafe {
        let extension = pg_sys::getExtensionOfObject(pg_sys::TypeRelationId, type_oid);
        if extension == pg_sys::InvalidOid {
            return false;
        }
        let name = pg_sys::get_extension_name(extension);
        if name.is_null() {
            return false;
        }
        let is_pgvector = CStr::from_ptr(name).to_bytes() == b"vector";
        pg_sys::pfree(name.cast());
        is_pgvector
    }
}

/// Emits a once-per-backend-per-index migration advisory when the index
/// serves a column whose type belongs to the pgvector extension.
///
/// Controlled by `pgcontext.pgvector_compat_warnings` (default on). Never
/// raises and never gates: full results are served either way.
pub(crate) fn nudge_pgvector_compat(index_relation: pg_sys::Relation) {
    if !crate::settings::pgvector_compat_warnings_from_guc() {
        return;
    }
    // SAFETY: every caller passes a locked, live index relation whose
    // rd_opcintype array is initialized by relcache for its indexed columns.
    let (index_oid, opcintype) = unsafe {
        if index_relation.is_null() {
            return;
        }
        let relation = &*index_relation;
        if relation.rd_opcintype.is_null() {
            return;
        }
        (relation.rd_id.to_u32(), *relation.rd_opcintype)
    };
    let already = PGVECTOR_NUDGED_INDEXES.with(|set| !set.borrow_mut().insert(index_oid));
    if already || !type_owned_by_pgvector(opcintype) {
        return;
    }
    pgrx::notice!(
        "pgcontext: this index serves a column typed by the pgvector extension; \
         results are complete, and pgcontext.migration_report() / \
         pgcontext.adopt_pgvector() can migrate the indexing when you are ready \
         (SET pgcontext.pgvector_compat_warnings = off to silence this notice)"
    );
}

fn map_pgvector_opclass(opclass: &str) -> Option<&'static str> {
    match opclass {
        "vector_l2_ops" => Some("pgcontext.vector_hnsw_pgvector_l2_ops"),
        "vector_cosine_ops" => Some("pgcontext.vector_hnsw_pgvector_cosine_ops"),
        "vector_ip_ops" => Some("pgcontext.vector_hnsw_pgvector_ip_ops"),
        "vector_l1_ops" => Some("pgcontext.vector_hnsw_pgvector_l1_ops"),
        "halfvec_l2_ops" => Some("pgcontext.halfvec_hnsw_pgvector_l2_ops"),
        "halfvec_ip_ops" => Some("pgcontext.halfvec_hnsw_pgvector_ip_ops"),
        "halfvec_cosine_ops" => Some("pgcontext.halfvec_hnsw_pgvector_cosine_ops"),
        "halfvec_l1_ops" => Some("pgcontext.halfvec_hnsw_pgvector_l1_ops"),
        "sparsevec_l2_ops" => Some("pgcontext.sparsevec_hnsw_pgvector_l2_ops"),
        "sparsevec_ip_ops" => Some("pgcontext.sparsevec_hnsw_pgvector_ip_ops"),
        "sparsevec_cosine_ops" => Some("pgcontext.sparsevec_hnsw_pgvector_cosine_ops"),
        "sparsevec_l1_ops" => Some("pgcontext.sparsevec_hnsw_pgvector_l1_ops"),
        _ => None,
    }
}

fn pgvector_binding_object_count() -> i64 {
    Spi::get_one::<i64>(
        "SELECT
           (SELECT count(*)
              FROM pg_catalog.pg_opclass AS opclass
              JOIN pg_catalog.pg_namespace AS namespace
                ON namespace.oid = opclass.opcnamespace
              JOIN pg_catalog.pg_am AS access_method
                ON access_method.oid = opclass.opcmethod
              JOIN pg_catalog.pg_extension AS extension
                ON extension.extname = 'pgcontext'
             WHERE namespace.nspname = 'pgcontext'
               AND access_method.amname = 'pgcontext_hnsw'
               AND opclass.opcowner = extension.extowner
               AND opclass.opcname LIKE '%_hnsw_pgvector_%_ops')
         + (SELECT count(*)
              FROM pg_catalog.pg_proc AS procedure
              JOIN pg_catalog.pg_namespace AS namespace
                ON namespace.oid = procedure.pronamespace
              JOIN pg_catalog.pg_extension AS extension
                ON extension.extname = 'pgcontext'
             WHERE namespace.nspname = 'pgcontext'
               AND procedure.proowner = extension.extowner
               AND (procedure.proname LIKE '_pgvector_%_support'
                    OR procedure.proname IN (
                        '_pgvector_sparsevec_to_pgcontext',
                        '_pgcontext_sparsevec_to_pgvector'
                    )))
         + (SELECT count(*)
              FROM pg_catalog.pg_cast AS cast_entry
             WHERE (cast_entry.castsource, cast_entry.casttarget) IN (
                   (pg_catalog.to_regtype('public.vector'), pg_catalog.to_regtype('pgcontext.vector')),
                   (pg_catalog.to_regtype('public.halfvec'), pg_catalog.to_regtype('pgcontext.halfvec')),
                   (pg_catalog.to_regtype('public.sparsevec'), pg_catalog.to_regtype('pgcontext.sparsevec')),
                   (pg_catalog.to_regtype('pgcontext.sparsevec'), pg_catalog.to_regtype('public.sparsevec'))
               ))",
    )
    .unwrap_or(Some(0))
    .unwrap_or(0)
}

pub(crate) fn pgvector_binding_installed() -> bool {
    Spi::get_one::<bool>(
        "SELECT
           (SELECT count(*) = 12 AND bool_and(pg_catalog.amvalidate(opclass.oid))
              FROM pg_catalog.pg_opclass AS opclass
              JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = opclass.opcnamespace
              JOIN pg_catalog.pg_am AS method ON method.oid = opclass.opcmethod
              JOIN pg_catalog.pg_extension AS extension ON extension.extname = 'pgcontext'
             WHERE namespace.nspname = 'pgcontext'
               AND method.amname = 'pgcontext_hnsw'
               AND opclass.opcowner = extension.extowner
               AND opclass.opcname = ANY (ARRAY[
                   'vector_hnsw_pgvector_l2_ops', 'vector_hnsw_pgvector_ip_ops',
                   'vector_hnsw_pgvector_cosine_ops', 'vector_hnsw_pgvector_l1_ops',
                   'halfvec_hnsw_pgvector_l2_ops', 'halfvec_hnsw_pgvector_ip_ops',
                   'halfvec_hnsw_pgvector_cosine_ops', 'halfvec_hnsw_pgvector_l1_ops',
                   'sparsevec_hnsw_pgvector_l2_ops', 'sparsevec_hnsw_pgvector_ip_ops',
                   'sparsevec_hnsw_pgvector_cosine_ops', 'sparsevec_hnsw_pgvector_l1_ops'
               ])
               AND (
                    opclass.opcname LIKE 'vector_%' AND opclass.opcintype = pg_catalog.to_regtype('public.vector')
                    OR opclass.opcname LIKE 'halfvec_%' AND opclass.opcintype = pg_catalog.to_regtype('public.halfvec')
                    OR opclass.opcname LIKE 'sparsevec_%' AND opclass.opcintype = pg_catalog.to_regtype('public.sparsevec')
               ))
         AND
           (SELECT count(*) = 12
              FROM pg_catalog.pg_proc AS procedure
              JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = procedure.pronamespace
              JOIN pg_catalog.pg_extension AS extension ON extension.extname = 'pgcontext'
             WHERE namespace.nspname = 'pgcontext'
               AND procedure.proowner = extension.extowner
               AND procedure.proname = ANY (ARRAY[
                   '_pgvector_vector_l2_support', '_pgvector_vector_ip_support',
                   '_pgvector_vector_cosine_support', '_pgvector_vector_l1_support',
                   '_pgvector_halfvec_l2_support', '_pgvector_halfvec_ip_support',
                   '_pgvector_halfvec_cosine_support', '_pgvector_halfvec_l1_support',
                   '_pgvector_sparsevec_l2_support', '_pgvector_sparsevec_ip_support',
                   '_pgvector_sparsevec_cosine_support', '_pgvector_sparsevec_l1_support'
               ])
               AND procedure.prorettype = 'pg_catalog.float8'::pg_catalog.regtype
               AND procedure.pronargs = 2
               AND procedure.proargtypes[0] = procedure.proargtypes[1]
               AND (
                    procedure.proname LIKE '_pgvector_vector_%' AND procedure.proargtypes[0] = pg_catalog.to_regtype('public.vector')
                    OR procedure.proname LIKE '_pgvector_halfvec_%' AND procedure.proargtypes[0] = pg_catalog.to_regtype('public.halfvec')
                    OR procedure.proname LIKE '_pgvector_sparsevec_%' AND procedure.proargtypes[0] = pg_catalog.to_regtype('public.sparsevec')
               ))
         AND
           (SELECT count(*) = 2
              FROM pg_catalog.pg_proc AS procedure
              JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = procedure.pronamespace
              JOIN pg_catalog.pg_extension AS extension ON extension.extname = 'pgcontext'
             WHERE namespace.nspname = 'pgcontext'
               AND procedure.proowner = extension.extowner
               AND (
                    procedure.proname = '_pgvector_sparsevec_to_pgcontext'
                    AND procedure.proargtypes[0] = pg_catalog.to_regtype('public.sparsevec')
                    AND procedure.prorettype = pg_catalog.to_regtype('pgcontext.sparsevec')
                    AND procedure.prosrc = 'pgcontext_pgvector_sparsevec_to_pgcontext'
                    OR procedure.proname = '_pgcontext_sparsevec_to_pgvector'
                    AND procedure.proargtypes[0] = pg_catalog.to_regtype('pgcontext.sparsevec')
                    AND procedure.prorettype = pg_catalog.to_regtype('public.sparsevec')
                    AND procedure.prosrc = 'pgcontext_pgcontext_sparsevec_to_pgvector'
               )
               AND procedure.pronargs = 1
               AND procedure.prolang = (SELECT oid FROM pg_catalog.pg_language WHERE lanname = 'c'))
         AND
           (SELECT count(*) = 4
              FROM pg_catalog.pg_cast AS cast_entry
             WHERE (cast_entry.castsource, cast_entry.casttarget, cast_entry.castmethod, cast_entry.castcontext, cast_entry.castfunc) IN (
                 (pg_catalog.to_regtype('public.vector'), pg_catalog.to_regtype('pgcontext.vector'), 'b', 'a', 0::pg_catalog.oid),
                 (pg_catalog.to_regtype('public.halfvec'), pg_catalog.to_regtype('pgcontext.halfvec'), 'b', 'a', 0::pg_catalog.oid),
                 (pg_catalog.to_regtype('public.sparsevec'), pg_catalog.to_regtype('pgcontext.sparsevec'), 'f', 'a', pg_catalog.to_regprocedure('pgcontext._pgvector_sparsevec_to_pgcontext(public.sparsevec)')),
                 (pg_catalog.to_regtype('pgcontext.sparsevec'), pg_catalog.to_regtype('public.sparsevec'), 'f', 'a', pg_catalog.to_regprocedure('pgcontext._pgcontext_sparsevec_to_pgvector(pgcontext.sparsevec)'))
             ))",
    )
    .unwrap_or(Some(false))
    .unwrap_or(false)
}

const MIGRATION_REPORT_SQL: &str = r"
WITH vector_columns AS (
    SELECT c.oid AS table_oid,
           n.nspname AS schema_name,
           c.relname AS table_name,
           c.relkind,
           a.attnum,
           a.attname AS column_name,
           a.attgenerated,
           a.atthasdef,
           t.typname AS declared_type_name,
           COALESCE(element_type.typname, t.typname) AS element_type_name,
           t.typelem <> 0 AS is_array,
           CASE WHEN a.atttypmod > 0 THEN a.atttypmod ELSE NULL END AS dimensions
      FROM pg_attribute a
      JOIN pg_class c ON c.oid = a.attrelid
      JOIN pg_namespace n ON n.oid = c.relnamespace
      JOIN pg_type t ON t.oid = a.atttypid
      LEFT JOIN pg_type element_type ON element_type.oid = t.typelem
     WHERE c.relkind IN ('r', 'p', 'm')
       AND a.attnum > 0
       AND NOT a.attisdropped
       AND n.nspname NOT IN ('pg_catalog', 'information_schema')
       AND (
         EXISTS (
           SELECT 1
             FROM pg_depend d
             JOIN pg_extension e ON e.oid = d.refobjid
            WHERE d.classid = 'pg_type'::regclass
              AND d.objid = t.oid
              AND d.deptype = 'e'
              AND e.extname = 'vector'
         )
         OR EXISTS (
           SELECT 1
             FROM pg_depend d
             JOIN pg_extension e ON e.oid = d.refobjid
            WHERE d.classid = 'pg_type'::regclass
              AND d.objid = t.typelem
              AND d.deptype = 'e'
              AND e.extname = 'vector'
         )
       )
), inventoried AS (
    SELECT vc.*,
           ARRAY_REMOVE(ARRAY[
             CASE WHEN vc.is_array
                  THEN 'array columns require element-wise conversion' END,
             CASE WHEN vc.element_type_name NOT IN ('vector', 'halfvec', 'sparsevec')
                  THEN format('physical layout for pgvector type %s is not certified',
                              vc.element_type_name) END,
             CASE WHEN vc.element_type_name = 'sparsevec'
                       AND vc.dimensions > 16000
                  THEN 'sparsevec dimensions exceed pgContext limit 16000' END,
             CASE WHEN vc.attgenerated <> ''
                  THEN 'generated columns are not supported' END,
             CASE WHEN vc.atthasdef
                  THEN 'column defaults must be migrated explicitly' END,
             CASE WHEN vc.relkind = 'p'
                  THEN 'partitioned parent tables are not supported' END,
             CASE WHEN EXISTS (
                    SELECT 1 FROM pg_inherits i WHERE i.inhrelid = vc.table_oid)
                  THEN 'table partitions must be migrated as a coordinated hierarchy' END,
             CASE WHEN EXISTS (
                    SELECT 1
                      FROM pg_partitioned_table p
                     WHERE p.partrelid = vc.table_oid
                       AND vc.attnum = ANY (p.partattrs))
                  THEN 'partition-key columns are not supported' END,
             CASE WHEN EXISTS (
                    SELECT 1
                      FROM pg_depend d
                      JOIN pg_rewrite r ON r.oid = d.objid
                      JOIN pg_class dependent ON dependent.oid = r.ev_class
                     WHERE d.classid = 'pg_rewrite'::regclass
                       AND d.refclassid = 'pg_class'::regclass
                       AND d.refobjid = vc.table_oid
                       AND d.refobjsubid = vc.attnum
                       AND dependent.relkind IN ('v', 'm'))
                  THEN 'dependent views or materialized views must be recreated' END,
             CASE WHEN EXISTS (
                    SELECT 1
                      FROM pg_index i
                     WHERE i.indrelid = vc.table_oid
                       AND vc.attnum = ANY (i.indkey)
                       AND (i.indexprs IS NOT NULL
                            OR i.indpred IS NOT NULL
                            OR i.indnkeyatts <> 1
                            OR i.indnatts <> 1))
                  THEN 'expression, partial, multicolumn, or INCLUDE indexes are not supported' END
           ], NULL)::text[] AS blockers
      FROM vector_columns vc
)
SELECT vc.schema_name::text,
       vc.table_name::text,
       vc.column_name::text,
       CASE WHEN vc.is_array
            THEN vc.element_type_name || '[]'
            ELSE vc.declared_type_name END::text AS type_name,
       vc.dimensions::int4,
       COALESCE((SELECT array_agg(ci.relname::text ORDER BY ci.relname)
                   FROM pg_index i
                   JOIN pg_class ci ON ci.oid = i.indexrelid
                   JOIN pg_am am ON am.oid = ci.relam
                  WHERE i.indrelid = vc.table_oid
                    AND am.amname IN ('hnsw', 'ivfflat')
                    AND i.indkey[0] = vc.attnum), '{}') AS pgvector_indexes,
       COALESCE((SELECT array_agg(ci.relname::text ORDER BY ci.relname)
                   FROM pg_index i
                   JOIN pg_class ci ON ci.oid = i.indexrelid
                   JOIN pg_am am ON am.oid = ci.relam
                  WHERE i.indrelid = vc.table_oid
                    AND am.amname = 'pgcontext_hnsw'
                    AND i.indkey[0] = vc.attnum), '{}') AS pgcontext_indexes,
       (SELECT o.opcname::text
          FROM pg_index i
          JOIN pg_class ci ON ci.oid = i.indexrelid
          JOIN pg_am am ON am.oid = ci.relam
          JOIN pg_opclass o ON o.oid = i.indclass[0]
         WHERE i.indrelid = vc.table_oid
           AND am.amname IN ('hnsw', 'ivfflat')
           AND i.indkey[0] = vc.attnum
         ORDER BY ci.relname
         LIMIT 1) AS first_pgvector_opclass,
       (SELECT am.amname::text
          FROM pg_index i
          JOIN pg_class ci ON ci.oid = i.indexrelid
          JOIN pg_am am ON am.oid = ci.relam
         WHERE i.indrelid = vc.table_oid
           AND am.amname IN ('hnsw', 'ivfflat')
           AND i.indkey[0] = vc.attnum
         ORDER BY ci.relname
         LIMIT 1) AS first_pgvector_access_method,
       cardinality(vc.blockers) = 0 AS conversion_supported,
       vc.blockers
  FROM inventoried vc
 ORDER BY 1, 2, 3";

/// Read-only inventory of pgvector-typed columns and how to migrate their
/// indexing to pgContext. Safe to run anywhere; touches only catalogs.
#[allow(
    clippy::type_complexity,
    reason = "pgrx column naming requires the name! tuple inline in the signature"
)]
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
fn migration_report() -> TableIterator<
    'static,
    (
        name!(schema_name, String),
        name!(table_name, String),
        name!(column_name, String),
        name!(type_name, String),
        name!(dimensions, Option<i32>),
        name!(pgvector_indexes, Vec<String>),
        name!(pgcontext_indexes, Vec<String>),
        name!(conversion_supported, bool),
        name!(blockers, Vec<String>),
        name!(suggested_command, String),
    ),
> {
    let rows = Spi::connect(|client| {
        let table = client
            .select(MIGRATION_REPORT_SQL, None, &[])
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to inventory pgvector columns: {error}"),
                )
            });
        let mut rows: Vec<MigrationReportRow> = Vec::new();
        for row in table {
            let schema: String = spi_required(row.get(1), "schema_name");
            let table_name: String = spi_required(row.get(2), "table_name");
            let column: String = spi_required(row.get(3), "column_name");
            let type_name: String = spi_required(row.get(4), "type_name");
            let dimensions: Option<i32> = row.get(5).unwrap_or(None);
            let pgvector_indexes: Vec<String> = row.get(6).unwrap_or(None).unwrap_or_default();
            let pgcontext_indexes: Vec<String> = row.get(7).unwrap_or(None).unwrap_or_default();
            let first_opclass: Option<String> = row.get(8).unwrap_or(None);
            let first_access_method: Option<String> = row.get(9).unwrap_or(None);
            let conversion_supported: bool = spi_required(row.get(10), "conversion_supported");
            let blockers: Vec<String> = row.get(11).unwrap_or(None).unwrap_or_default();
            let suggested = suggest_command(
                &schema,
                &table_name,
                &column,
                &type_name,
                first_access_method.as_deref(),
                first_opclass.as_deref(),
                !pgcontext_indexes.is_empty(),
            );
            rows.push((
                schema,
                table_name,
                column,
                type_name,
                dimensions,
                pgvector_indexes,
                pgcontext_indexes,
                conversion_supported,
                blockers,
                suggested,
            ));
        }
        rows
    });
    TableIterator::new(rows)
}

fn spi_required<T>(value: Result<Option<T>, spi::Error>, column: &'static str) -> T {
    match value {
        Ok(Some(inner)) => inner,
        Ok(None) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("migration report column is unexpectedly null: {column}"),
        ),
        Err(error) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to read migration report column {column}: {error}"),
        ),
    }
}

fn quote_ident(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn suggest_command(
    schema: &str,
    table: &str,
    column: &str,
    type_name: &str,
    first_access_method: Option<&str>,
    first_opclass: Option<&str>,
    has_pgcontext_index: bool,
) -> String {
    if has_pgcontext_index {
        return "already indexed by pgcontext_hnsw".to_owned();
    }
    if first_access_method == Some("ivfflat") {
        return format!(
            "SELECT * FROM pgcontext.start_pgvector_ownership_conversion(\
             '{}.{}'::regclass, '{}', 'restricted_online', '<metric>', \
             application_uses_column_lists => true, \
             application_dependencies_reviewed => true); \
             pgvector IVFFlat is rebuilt as pgcontext_ivfflat after validated ownership conversion",
            quote_ident(schema),
            quote_ident(table),
            column.replace('\'', "''"),
        );
    }
    let opclass = match first_opclass {
        Some(name) => match map_pgvector_opclass(name) {
            Some(mapped) => mapped,
            None => {
                return format!(
                    "no pgContext opclass equivalent for pgvector opclass {name}; \
                     choose a metric and index manually"
                );
            }
        },
        None => match type_name {
            "vector" => "pgcontext.vector_hnsw_pgvector_cosine_ops",
            "halfvec" => "pgcontext.halfvec_hnsw_pgvector_cosine_ops",
            "sparsevec" => "pgcontext.sparsevec_hnsw_pgvector_cosine_ops",
            _ => {
                return format!(
                    "no certified pgContext opclass binding for pgvector type {type_name}"
                );
            }
        },
    };
    format!(
        "CREATE INDEX CONCURRENTLY ON {}.{} USING pgcontext_hnsw ({} {})",
        quote_ident(schema),
        quote_ident(table),
        quote_ident(column),
        opclass,
    )
}

const ADOPT_INDEXES_SQL: &str = r"
SELECT n.nspname::text AS schema_name,
       ct.relname::text AS table_name,
       ci.relname::text AS index_name,
       ci.oid::bigint AS index_oid,
       a.attname::text AS column_name,
       o.opcname::text AS opclass_name,
       am.amname::text AS am_name,
       i.indnkeyatts::int4,
       i.indnatts::int4,
       i.indexprs IS NOT NULL AS expression_index,
       i.indpred IS NOT NULL AS partial_index,
       i.indisvalid AND i.indisready AND i.indislive AS usable,
       ci.relkind = 'I' AS partitioned_index,
       COALESCE(ci.reloptions, '{}')::text[] AS reloptions,
       NULLIF(ts.spcname, 'pg_default')::text AS tablespace
  FROM pg_index i
  JOIN pg_class ci ON ci.oid = i.indexrelid
  JOIN pg_class ct ON ct.oid = i.indrelid
  JOIN pg_namespace n ON n.oid = ct.relnamespace
  JOIN pg_am am ON am.oid = ci.relam
  JOIN pg_opclass o ON o.oid = i.indclass[0]
  JOIN pg_depend od ON od.classid = 'pg_opclass'::regclass
                   AND od.objid = o.oid AND od.deptype = 'e'
  JOIN pg_extension oe ON oe.oid = od.refobjid AND oe.extname = 'vector'
  LEFT JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = i.indkey[0]
  LEFT JOIN pg_tablespace ts ON ts.oid = ci.reltablespace
 WHERE am.amname IN ('hnsw', 'ivfflat')
   AND ($1::oid IS NULL OR i.indrelid = $1::oid)
 ORDER BY 1, 2, 3";

fn truncate_identifier_prefix(value: &str, max_bytes: usize) -> &str {
    let mut end = value.len().min(max_bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn replacement_index_name(schema: &str, source_name: &str, source_oid: i64) -> String {
    for attempt in 0_u16..=1000 {
        let suffix = if attempt == 0 {
            format!("_pgc_{source_oid:x}")
        } else {
            format!("_pgc_{source_oid:x}_{attempt}")
        };
        let prefix = truncate_identifier_prefix(source_name, 63_usize.saturating_sub(suffix.len()));
        let candidate = format!("{prefix}{suffix}");
        let available = Spi::get_one_with_args::<bool>(
            "SELECT pg_catalog.to_regclass(
                        pg_catalog.format('%I.%I', $1, $2)
                    ) IS NULL",
            &[schema.into(), candidate.clone().into()],
        )
        .unwrap_or(Some(false))
        .unwrap_or(false);
        if available {
            return candidate;
        }
    }
    raise_sql_error(
        PgSqlErrorCode::ERRCODE_DUPLICATE_OBJECT,
        "could not allocate a collision-free pgContext replacement index name",
    )
}

/// Migrates pgvector `hnsw`/`ivfflat` indexes to `pgcontext_hnsw`
/// equivalents. `dry_run` (the default) only reports the commands;
/// `drop_old` additionally drops each pgvector index after its
/// replacement builds. Column types are untouched. Executable replacement
/// commands require the separately installed pgvector bridge; ownership
/// conversion is a distinct workflow.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
fn adopt_pgvector(
    target: default!(Option<PgRelation>, "NULL"),
    dry_run: default!(bool, true),
    drop_old: default!(bool, false),
) -> TableIterator<
    'static,
    (
        name!(index_name, String),
        name!(action, String),
        name!(command, String),
        name!(executed, bool),
    ),
> {
    if !dry_run && !pgvector_binding_installed() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
            "executing a pgvector index-adoption plan requires main-extension \
             pgvector bindings; run pgcontext.enable_pgvector_binding() as the \
             pgcontext extension owner, or keep dry_run => true",
        );
    }
    let target_oid: Option<pg_sys::Oid> = target.as_ref().map(|relation| relation.oid());
    // `PgRelation` keeps the target open for as long as the wrapper lives.
    // Release it before an executable plan issues CREATE INDEX on that table;
    // PostgreSQL rejects DDL against a relation still used by an active query
    // in the same session.
    drop(target);
    let plans = Spi::connect(|client| {
        let table = client
            .select(ADOPT_INDEXES_SQL, None, &[target_oid.into()])
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to inventory pgvector indexes: {error}"),
                )
            });
        let mut plans = Vec::new();
        for row in table {
            let schema: String = spi_required(row.get(1), "schema_name");
            let table_name: String = spi_required(row.get(2), "table_name");
            let index: String = spi_required(row.get(3), "index_name");
            let index_oid: i64 = spi_required(row.get(4), "index_oid");
            let column: Option<String> = row.get(5).unwrap_or(None);
            let opclass: String = spi_required(row.get(6), "opclass_name");
            let access_method: String = spi_required(row.get(7), "am_name");
            let key_columns: i32 = spi_required(row.get(8), "indnkeyatts");
            let total_columns: i32 = spi_required(row.get(9), "indnatts");
            let expression_index: bool = spi_required(row.get(10), "expression_index");
            let partial_index: bool = spi_required(row.get(11), "partial_index");
            let usable: bool = spi_required(row.get(12), "usable");
            let partitioned_index: bool = spi_required(row.get(13), "partitioned_index");
            let reloptions: Vec<String> = row.get(14).unwrap_or(None).unwrap_or_default();
            let tablespace: Option<String> = row.get(15).unwrap_or(None);
            plans.push((
                schema,
                table_name,
                index,
                index_oid,
                column,
                opclass,
                access_method,
                key_columns,
                total_columns,
                expression_index,
                partial_index,
                usable,
                partitioned_index,
                reloptions,
                tablespace,
            ));
        }
        plans
    });

    let mut rows = Vec::new();
    for (
        schema,
        table_name,
        index,
        index_oid,
        column,
        opclass,
        access_method,
        key_columns,
        total_columns,
        expression_index,
        partial_index,
        usable,
        partitioned_index,
        reloptions,
        tablespace,
    ) in plans
    {
        let Some(column) = column else {
            rows.push((
                index,
                "skipped: expression index (no plain indexed column)".to_owned(),
                String::new(),
                false,
            ));
            continue;
        };
        let refusal = if expression_index {
            Some("expression index")
        } else if partial_index {
            Some("partial index")
        } else if key_columns != 1 || total_columns != 1 {
            Some("multicolumn or INCLUDE index")
        } else if !usable {
            Some("invalid, unready, or non-live index")
        } else if partitioned_index {
            Some("partitioned parent index")
        } else if access_method == "ivfflat" {
            Some(
                "IVFFlat requires validated ownership conversion to pgcontext_ivfflat; it is never remapped to HNSW",
            )
        } else if reloptions
            .iter()
            .any(|option| !option.starts_with("m=") && !option.starts_with("ef_construction="))
        {
            Some("unrecognized HNSW reloptions")
        } else {
            None
        };
        if let Some(reason) = refusal {
            rows.push((index, format!("skipped: {reason}"), String::new(), false));
            continue;
        }
        let Some(mapped) = map_pgvector_opclass(&opclass) else {
            rows.push((
                index,
                format!("skipped: no pgContext equivalent for opclass {opclass}"),
                String::new(),
                false,
            ));
            continue;
        };
        let new_index = replacement_index_name(&schema, &index, index_oid);
        let options = if reloptions.is_empty() {
            String::new()
        } else {
            format!(" WITH ({})", reloptions.join(", "))
        };
        let tablespace_clause = tablespace
            .as_deref()
            .map(|name| format!(" TABLESPACE {}", quote_ident(name)))
            .unwrap_or_default();
        let create = format!(
            "CREATE INDEX {} ON {}.{} USING pgcontext_hnsw ({} {}){}{}",
            quote_ident(&new_index),
            quote_ident(&schema),
            quote_ident(&table_name),
            quote_ident(&column),
            mapped,
            options,
            tablespace_clause,
        );
        if dry_run {
            rows.push((index.clone(), "would create".to_owned(), create, false));
            if drop_old {
                rows.push((
                    index.clone(),
                    "would drop".to_owned(),
                    format!(
                        "DROP INDEX {}.{}",
                        quote_ident(&schema),
                        quote_ident(&index)
                    ),
                    false,
                ));
            }
            continue;
        }
        Spi::run(&create).unwrap_or_else(|error| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to create replacement index {new_index}: {error}"),
            )
        });
        rows.push((index.clone(), "created".to_owned(), create, true));
        if drop_old {
            let qualified_table = format!("{}.{}", quote_ident(&schema), quote_ident(&table_name));
            let validated: Option<bool> = Spi::get_one_with_args(
                "SELECT bool_and(recall_at_10 >= 0.99)\n\
                   FROM pgcontext.compare_indexes($1, $2, 20)\n\
                  WHERE index_name = $3\n\
                    AND recall_at_10 IS NOT NULL",
                &[
                    qualified_table.into(),
                    column.clone().into(),
                    new_index.clone().into(),
                ],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to validate replacement index {new_index}: {error}"),
                )
            });
            if validated != Some(true) {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_EXCEPTION,
                    format!(
                        "replacement index {new_index} did not meet the exact-oracle recall gate; source index {index} was not dropped"
                    ),
                );
            }
            rows.push((
                new_index.clone(),
                "validated against exact oracle".to_owned(),
                "required recall_at_10 >= 0.99".to_owned(),
                true,
            ));
            let drop = format!(
                "DROP INDEX {}.{}",
                quote_ident(&schema),
                quote_ident(&index)
            );
            Spi::run(&drop).unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to drop pgvector index {index}: {error}"),
                )
            });
            rows.push((index, "dropped".to_owned(), drop, true));
        }
    }
    TableIterator::new(rows)
}

fn require_pgvector_binding_owner() {
    let allowed = Spi::get_one::<bool>(
        "SELECT extension.extowner = role.oid
           FROM pg_catalog.pg_extension AS extension
           JOIN pg_catalog.pg_roles AS role
             ON role.rolname = CURRENT_USER
          WHERE extension.extname = 'pgcontext'",
    )
    .unwrap_or(Some(false))
    .unwrap_or(false);
    if !allowed {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            "must SET ROLE to the pgcontext extension owner to change pgvector compatibility objects",
        );
    }
}

fn require_pgvector_facade_owner() {
    require_pgvector_binding_owner();
    let is_superuser = Spi::get_one::<bool>(
        "SELECT usesuper FROM pg_catalog.pg_user WHERE usename = CURRENT_USER",
    )
    .unwrap_or(Some(false))
    .unwrap_or(false);
    if !is_superuser {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            "pgvector access-method facade changes require the superuser pgcontext extension owner",
        );
    }
}

fn require_pgvector_binding_profile() {
    let profile = Spi::get_one::<bool>(
        "SELECT extension.extversion ~ '^0[.]8[.][0-9]+$'
                AND namespace.nspname = 'public'
           FROM pg_catalog.pg_extension AS extension
           JOIN pg_catalog.pg_namespace AS namespace
             ON namespace.oid = extension.extnamespace
          WHERE extension.extname = 'vector'",
    )
    .unwrap_or(None);
    match profile {
        Some(true) => {}
        Some(false) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
            "pgvector bindings certify pgvector 0.8.x installed in public",
        ),
        None => raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
            "pgvector extension 'vector' is not installed",
        ),
    }
    let retired_companion = Spi::get_one::<bool>(
        "SELECT EXISTS (
             SELECT 1 FROM pg_catalog.pg_extension
              WHERE extname = 'pgcontext_pgvector'
         )",
    )
    .unwrap_or(Some(false))
    .unwrap_or(false);
    if retired_companion {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_IN_USE,
            "drop the retired pgcontext_pgvector companion extension before enabling main-extension bindings",
        );
    }
}

fn run_binding_sql(sql: &str, description: &str) {
    PgTryBuilder::new(|| Spi::run(sql))
        .catch_others(|cause| cause.rethrow())
        .execute()
        .unwrap_or_else(|error| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to {description}: {error}"),
            )
        });
}

fn create_binding_cast(source: &str, target: &str, clause: &str) {
    run_binding_sql(
        &format!("CREATE CAST ({source} AS {target}) {clause}"),
        "create pgvector compatibility cast",
    );
}

fn create_binding_function(_name: &str, _arguments: &str, definition: &str) {
    run_binding_sql(definition, "create pgvector compatibility function");
}

fn create_pgvector_metric_binding(type_name: &str, metric: &str, operator: &str, expression: &str) {
    let function_name = format!("_pgvector_{type_name}_{metric}_support");
    let arguments = format!("public.{type_name}, public.{type_name}");
    create_binding_function(
        &function_name,
        &arguments,
        &format!(
            "CREATE FUNCTION pgcontext.{function_name}(public.{type_name}, public.{type_name}) \
             RETURNS double precision LANGUAGE SQL IMMUTABLE STRICT PARALLEL SAFE \
             SET search_path = pg_catalog, pg_temp RETURN {expression}"
        ),
    );
    let opclass_name = format!("{type_name}_hnsw_pgvector_{metric}_ops");
    run_binding_sql(
        &format!(
            "CREATE OPERATOR CLASS pgcontext.{opclass_name} \
             FOR TYPE public.{type_name} USING pgcontext_hnsw AS \
             OPERATOR 1 public.{operator} (public.{type_name}, public.{type_name}) \
                 FOR ORDER BY pg_catalog.float_ops, \
             FUNCTION 1 pgcontext.{function_name}(public.{type_name}, public.{type_name}), \
             STORAGE pgcontext.vector"
        ),
        "create pgvector compatibility opclass",
    );
}

fn remove_binding_object(drop_sql: &str, description: &str) {
    run_binding_sql(drop_sql, &format!("drop {description}"));
}

fn install_pgvector_binding_objects() {
    create_binding_cast(
        "public.vector",
        "pgcontext.vector",
        "WITHOUT FUNCTION AS ASSIGNMENT",
    );
    create_binding_cast(
        "public.halfvec",
        "pgcontext.halfvec",
        "WITHOUT FUNCTION AS ASSIGNMENT",
    );
    create_binding_function(
        "_pgvector_sparsevec_to_pgcontext",
        "public.sparsevec",
        "CREATE FUNCTION pgcontext._pgvector_sparsevec_to_pgcontext(public.sparsevec) \
         RETURNS pgcontext.sparsevec AS '$libdir/pgcontext', \
         'pgcontext_pgvector_sparsevec_to_pgcontext' \
         LANGUAGE C IMMUTABLE STRICT PARALLEL SAFE",
    );
    create_binding_function(
        "_pgcontext_sparsevec_to_pgvector",
        "pgcontext.sparsevec",
        "CREATE FUNCTION pgcontext._pgcontext_sparsevec_to_pgvector(pgcontext.sparsevec) \
         RETURNS public.sparsevec AS '$libdir/pgcontext', \
         'pgcontext_pgcontext_sparsevec_to_pgvector' \
         LANGUAGE C IMMUTABLE STRICT PARALLEL SAFE",
    );
    create_binding_cast(
        "public.sparsevec",
        "pgcontext.sparsevec",
        "WITH FUNCTION pgcontext._pgvector_sparsevec_to_pgcontext(public.sparsevec) AS ASSIGNMENT",
    );
    create_binding_cast(
        "pgcontext.sparsevec",
        "public.sparsevec",
        "WITH FUNCTION pgcontext._pgcontext_sparsevec_to_pgvector(pgcontext.sparsevec) AS ASSIGNMENT",
    );

    let metric_bindings = [
        ("l2", "<->"),
        ("ip", "<#>"),
        ("cosine", "<=>"),
        ("l1", "<+>"),
    ];
    for (metric, operator) in metric_bindings {
        let vector_expression = match metric {
            "l2" => "pgcontext.hnsw_l2_distance($1::pgcontext.vector, $2::pgcontext.vector)",
            "ip" => {
                "pgcontext.negative_inner_product($1::pgcontext.vector, $2::pgcontext.vector)::double precision"
            }
            "cosine" => {
                "pgcontext.cosine_distance($1::pgcontext.vector, $2::pgcontext.vector)::double precision"
            }
            "l1" => {
                "pgcontext.l1_distance($1::pgcontext.vector, $2::pgcontext.vector)::double precision"
            }
            _ => unreachable!("static pgvector metric table is exhaustive"),
        };
        create_pgvector_metric_binding("vector", metric, operator, vector_expression);

        let halfvec_expression = match metric {
            "l2" => {
                "pgcontext.halfvec_l2_distance($1::pgcontext.halfvec, $2::pgcontext.halfvec)::double precision"
            }
            "ip" => {
                "pgcontext.halfvec_negative_inner_product($1::pgcontext.halfvec, $2::pgcontext.halfvec)::double precision"
            }
            "cosine" => {
                "pgcontext.halfvec_cosine_distance($1::pgcontext.halfvec, $2::pgcontext.halfvec)::double precision"
            }
            "l1" => {
                "pgcontext.halfvec_l1_distance($1::pgcontext.halfvec, $2::pgcontext.halfvec)::double precision"
            }
            _ => unreachable!("static pgvector metric table is exhaustive"),
        };
        create_pgvector_metric_binding("halfvec", metric, operator, halfvec_expression);

        let sparsevec_expression = match metric {
            "l2" => "public.l2_distance($1, $2)",
            "ip" => "-public.inner_product($1, $2)",
            "cosine" => "public.cosine_distance($1, $2)",
            "l1" => "public.l1_distance($1, $2)",
            _ => unreachable!("static pgvector metric table is exhaustive"),
        };
        create_pgvector_metric_binding("sparsevec", metric, operator, sparsevec_expression);
    }
}

/// Installs dump-visible pgvector coexistence bindings owned by the exact main
/// extension owner. Repeating a completed installation is a no-op.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
fn enable_pgvector_binding() {
    require_pgvector_binding_owner();
    require_pgvector_binding_profile();
    if pgvector_binding_installed() {
        return;
    }
    let conflicting_objects = Spi::get_one::<i64>(
        "SELECT count(*)::bigint
           FROM pg_catalog.pg_opclass AS opclass
           JOIN pg_catalog.pg_namespace AS namespace
             ON namespace.oid = opclass.opcnamespace
          WHERE namespace.nspname = 'pgcontext'
            AND opclass.opcname LIKE '%_hnsw_pgvector_%_ops'",
    )
    .unwrap_or(Some(0))
    .unwrap_or(0);
    if pgvector_binding_object_count() != 0 || conflicting_objects != 0 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DUPLICATE_OBJECT,
            "partial or foreign pgvector compatibility objects already exist; remove them before enabling the pgcontext-owned binding",
        );
    }
    install_pgvector_binding_objects();
}

/// Removes the optional pgvector-owned-type bindings.
/// PostgreSQL refuses the operation while a live index still uses an opclass.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
fn disable_pgvector_binding() {
    require_pgvector_binding_owner();
    let object_count = pgvector_binding_object_count();
    if object_count == 0 {
        return;
    }
    if object_count != 30 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "pgvector compatibility binding is only partially owned by pgcontext",
        );
    }
    for type_name in ["vector", "halfvec", "sparsevec"] {
        for metric in ["l2", "ip", "cosine", "l1"] {
            let opclass_name = format!("{type_name}_hnsw_pgvector_{metric}_ops");
            remove_binding_object(
                &format!(
                    "DROP OPERATOR CLASS pgcontext.{opclass_name} USING pgcontext_hnsw RESTRICT"
                ),
                "pgvector compatibility opclass",
            );
        }
    }
    for (source, target) in [
        ("public.vector", "pgcontext.vector"),
        ("public.halfvec", "pgcontext.halfvec"),
        ("public.sparsevec", "pgcontext.sparsevec"),
        ("pgcontext.sparsevec", "public.sparsevec"),
    ] {
        remove_binding_object(
            &format!("DROP CAST ({source} AS {target})"),
            "pgvector compatibility cast",
        );
    }
    for (name, arguments) in [
        ("_pgvector_sparsevec_to_pgcontext", "public.sparsevec"),
        ("_pgcontext_sparsevec_to_pgvector", "pgcontext.sparsevec"),
    ] {
        remove_binding_object(
            &format!("DROP FUNCTION pgcontext.{name}({arguments}) RESTRICT"),
            "pgvector compatibility function",
        );
    }
    for type_name in ["vector", "halfvec", "sparsevec"] {
        for metric in ["l2", "ip", "cosine", "l1"] {
            let name = format!("_pgvector_{type_name}_{metric}_support");
            let arguments = format!("public.{type_name}, public.{type_name}");
            remove_binding_object(
                &format!("DROP FUNCTION pgcontext.{name}({arguments}) RESTRICT"),
                "pgvector compatibility function",
            );
        }
    }
}

fn facade_access_methods_owned_by_pgcontext() -> i64 {
    Spi::get_one::<i64>(
        "SELECT count(*)::bigint
           FROM pg_catalog.pg_am AS access_method
           JOIN pg_catalog.pg_proc AS handler
             ON handler.oid = access_method.amhandler
           JOIN pg_catalog.pg_namespace AS namespace
             ON namespace.oid = handler.pronamespace
          WHERE namespace.nspname = 'pgcontext'
            AND (
                 access_method.amname = 'hnsw' AND handler.proname = 'hnsw_handler'
                 OR access_method.amname = 'ivfflat' AND handler.proname = 'ivfflat_handler'
            )",
    )
    .unwrap_or(Some(0))
    .unwrap_or(0)
}

fn facade_opclasses_owned_by_pgcontext() -> i64 {
    Spi::get_one::<i64>(
        "SELECT count(*)::bigint
           FROM pg_catalog.pg_opclass AS opclass
           JOIN pg_catalog.pg_am AS access_method
             ON access_method.oid = opclass.opcmethod
           JOIN pg_catalog.pg_namespace AS namespace
             ON namespace.oid = opclass.opcnamespace
           JOIN pg_catalog.pg_extension AS extension
             ON extension.extname = 'pgcontext'
          WHERE access_method.amname IN ('hnsw', 'ivfflat')
            AND namespace.nspname = 'pgcontext'
            AND opclass.opcowner = extension.extowner",
    )
    .unwrap_or(Some(0))
    .unwrap_or(0)
}

fn facade_opclasses_are_exact() -> bool {
    Spi::get_one::<bool>(
        "WITH expected(method_name, type_name, opclass_name) AS (
             VALUES
               ('hnsw', 'vector', 'vector_l2_ops'),
               ('hnsw', 'vector', 'vector_ip_ops'),
               ('hnsw', 'vector', 'vector_cosine_ops'),
               ('hnsw', 'vector', 'vector_l1_ops'),
               ('hnsw', 'halfvec', 'halfvec_l2_ops'),
               ('hnsw', 'halfvec', 'halfvec_ip_ops'),
               ('hnsw', 'halfvec', 'halfvec_cosine_ops'),
               ('hnsw', 'halfvec', 'halfvec_l1_ops'),
               ('hnsw', 'sparsevec', 'sparsevec_l2_ops'),
               ('hnsw', 'sparsevec', 'sparsevec_ip_ops'),
               ('hnsw', 'sparsevec', 'sparsevec_cosine_ops'),
               ('hnsw', 'sparsevec', 'sparsevec_l1_ops'),
               ('hnsw', 'bitvec', 'bit_hamming_ops'),
               ('hnsw', 'bitvec', 'bit_jaccard_ops'),
               ('ivfflat', 'vector', 'vector_l2_ops'),
               ('ivfflat', 'vector', 'vector_ip_ops'),
               ('ivfflat', 'vector', 'vector_cosine_ops'),
               ('ivfflat', 'vector', 'vector_l1_ops'),
               ('ivfflat', 'halfvec', 'halfvec_l2_ops'),
               ('ivfflat', 'halfvec', 'halfvec_ip_ops'),
               ('ivfflat', 'halfvec', 'halfvec_cosine_ops'),
               ('ivfflat', 'halfvec', 'halfvec_l1_ops'),
               ('ivfflat', 'bitvec', 'bit_hamming_ops'),
               ('ivfflat', 'bitvec', 'bit_jaccard_ops')
         ), actual AS (
             SELECT method.amname::text AS method_name,
                    type.typname::text AS type_name,
                    opclass.opcname::text AS opclass_name,
                    opclass.oid
               FROM pg_catalog.pg_opclass AS opclass
               JOIN pg_catalog.pg_am AS method ON method.oid = opclass.opcmethod
               JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = opclass.opcnamespace
               JOIN pg_catalog.pg_type AS type ON type.oid = opclass.opcintype
               JOIN pg_catalog.pg_namespace AS type_namespace ON type_namespace.oid = type.typnamespace
               JOIN pg_catalog.pg_extension AS extension ON extension.extname = 'pgcontext'
              WHERE method.amname IN ('hnsw', 'ivfflat')
                AND namespace.nspname = 'pgcontext'
                AND type_namespace.nspname = 'pgcontext'
                AND opclass.opcowner = extension.extowner
         )
         SELECT (SELECT count(*) = 24 AND bool_and(pg_catalog.amvalidate(oid)) FROM actual)
            AND NOT EXISTS (
                SELECT method_name, type_name, opclass_name FROM expected
                EXCEPT
                SELECT method_name, type_name, opclass_name FROM actual
            )
            AND NOT EXISTS (
                SELECT method_name, type_name, opclass_name FROM actual
                EXCEPT
                SELECT method_name, type_name, opclass_name FROM expected
            )",
    )
    .unwrap_or(Some(false))
    .unwrap_or(false)
}

fn create_name_facade_opclass(
    access_method: &str,
    type_name: &str,
    opclass_name: &str,
    operator: &str,
    support_function: &str,
    order_family: &str,
    storage: bool,
) {
    let storage_clause = if storage {
        ", STORAGE pgcontext.vector"
    } else {
        ""
    };
    run_binding_sql(
        &format!(
            "CREATE OPERATOR CLASS pgcontext.{opclass_name} \
             FOR TYPE pgcontext.{type_name} USING {access_method} AS \
             OPERATOR 1 pgcontext.{operator} (pgcontext.{type_name}, pgcontext.{type_name}) \
                 FOR ORDER BY pg_catalog.{order_family}, \
             FUNCTION 1 pgcontext.{support_function}(pgcontext.{type_name}, pgcontext.{type_name})\
             {storage_clause}"
        ),
        "create pgvector name-facade opclass",
    );
}

fn install_name_facade_opclasses() {
    let dense_metrics = [
        ("vector_l2_ops", "<->", "hnsw_l2_distance"),
        ("vector_ip_ops", "<#>", "negative_inner_product"),
        ("vector_cosine_ops", "<=>", "cosine_distance"),
        ("vector_l1_ops", "<+>", "l1_distance"),
    ];
    let half_metrics = [
        ("halfvec_l2_ops", "<->", "halfvec_l2_distance"),
        ("halfvec_ip_ops", "<#>", "halfvec_negative_inner_product"),
        ("halfvec_cosine_ops", "<=>", "halfvec_cosine_distance"),
        ("halfvec_l1_ops", "<+>", "halfvec_l1_distance"),
    ];
    for access_method in ["hnsw", "ivfflat"] {
        for (name, operator, function) in dense_metrics {
            create_name_facade_opclass(
                access_method,
                "vector",
                name,
                operator,
                function,
                "float_ops",
                false,
            );
        }
        for (name, operator, function) in half_metrics {
            create_name_facade_opclass(
                access_method,
                "halfvec",
                name,
                operator,
                function,
                "float_ops",
                true,
            );
        }
    }
    for (name, operator, function) in [
        ("sparsevec_l2_ops", "<->", "sparsevec_l2_distance"),
        (
            "sparsevec_ip_ops",
            "<#>",
            "sparsevec_negative_inner_product",
        ),
        ("sparsevec_cosine_ops", "<=>", "sparsevec_cosine_distance"),
        ("sparsevec_l1_ops", "<+>", "sparsevec_l1_distance"),
    ] {
        create_name_facade_opclass(
            "hnsw",
            "sparsevec",
            name,
            operator,
            function,
            "float_ops",
            true,
        );
    }
    for access_method in ["hnsw", "ivfflat"] {
        create_name_facade_opclass(
            access_method,
            "bitvec",
            "bit_hamming_ops",
            "<~>",
            "bitvec_hamming_distance",
            "integer_ops",
            true,
        );
        create_name_facade_opclass(
            access_method,
            "bitvec",
            "bit_jaccard_ops",
            "<%>",
            "bitvec_jaccard_distance",
            "float_ops",
            true,
        );
    }
}

/// Installs unqualified `hnsw` and `ivfflat` access-method aliases only when
/// neither name is owned by another extension. Put `pgcontext` on search_path
/// to use the pgvector-spelled opclass aliases with canonical pgContext types.
#[pg_extern]
#[search_path(pg_catalog, pgcontext)]
fn enable_pgvector_name_facade() {
    require_pgvector_facade_owner();
    let owned_access_methods = facade_access_methods_owned_by_pgcontext();
    let owned_opclasses = facade_opclasses_owned_by_pgcontext();
    if owned_access_methods == 2 && owned_opclasses == 24 && facade_opclasses_are_exact() {
        return;
    }
    if owned_access_methods != 0 || owned_opclasses != 0 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "pgvector name facade is only partially owned by pgcontext",
        );
    }
    let conflicting_names = Spi::get_one::<i64>(
        "SELECT count(*)::bigint
           FROM pg_catalog.pg_am
          WHERE amname IN ('hnsw', 'ivfflat')",
    )
    .unwrap_or(Some(0))
    .unwrap_or(0);
    if conflicting_names != 0 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DUPLICATE_OBJECT,
            "cannot install pgvector name facade because hnsw or ivfflat is already owned; use pgcontext_hnsw and pgcontext_ivfflat explicitly",
        );
    }
    for (name, handler) in [
        ("hnsw", "pgcontext.hnsw_handler"),
        ("ivfflat", "pgcontext.ivfflat_handler"),
    ] {
        run_binding_sql(
            &format!("CREATE ACCESS METHOD {name} TYPE INDEX HANDLER {handler}"),
            "create pgvector access-method facade",
        );
    }
    install_name_facade_opclasses();
}

/// Removes the optional unqualified access-method aliases. Existing facade
/// indexes make PostgreSQL fail with dependent-objects SQLSTATE `2BP01`.
#[pg_extern]
#[search_path(pg_catalog, pgcontext)]
fn disable_pgvector_name_facade() {
    require_pgvector_facade_owner();
    let owned_access_methods = facade_access_methods_owned_by_pgcontext();
    let owned_opclasses = facade_opclasses_owned_by_pgcontext();
    if owned_access_methods == 0 && owned_opclasses == 0 {
        return;
    }
    if owned_access_methods != 2 || owned_opclasses != 24 || !facade_opclasses_are_exact() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "pgvector name facade is only partially owned by pgcontext",
        );
    }
    for access_method in ["hnsw", "ivfflat"] {
        let opclasses = Spi::connect(|client| {
            client
                .select(
                    "SELECT namespace.nspname::text, opclass.opcname::text
                       FROM pg_catalog.pg_opclass AS opclass
                       JOIN pg_catalog.pg_namespace AS namespace
                         ON namespace.oid = opclass.opcnamespace
                       JOIN pg_catalog.pg_am AS method
                         ON method.oid = opclass.opcmethod
                      WHERE method.amname = $1
                        AND namespace.nspname = 'pgcontext'
                      ORDER BY namespace.nspname, opclass.opcname",
                    None,
                    &[access_method.into()],
                )?
                .map(|row| {
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
                format!("failed to inventory pgvector facade opclasses: {error}"),
            )
        });
        for (namespace, name) in opclasses {
            remove_binding_object(
                &format!(
                    "DROP OPERATOR CLASS {}.{} USING {access_method} RESTRICT",
                    quote_ident(&namespace),
                    quote_ident(&name),
                ),
                "pgvector name-facade opclass",
            );
        }
    }
    for name in ["hnsw", "ivfflat"] {
        remove_binding_object(
            &format!("DROP ACCESS METHOD {name} RESTRICT"),
            "pgvector access-method facade",
        );
    }
}

/// One ANN operator family reachable on a column: the operator spelling
/// determines which extension's index the planner may use, so each family
/// is measured separately and attributed to the planner-chosen index.
struct OperatorFamilyRun {
    operator: &'static str,
}

const COMPARE_OPERATOR_FAMILIES: &[OperatorFamilyRun] = &[
    OperatorFamilyRun {
        operator: "OPERATOR(public.<->)",
    },
    OperatorFamilyRun {
        operator: "OPERATOR(public.<=>)",
    },
    OperatorFamilyRun {
        operator: "OPERATOR(public.<#>)",
    },
    OperatorFamilyRun {
        operator: "OPERATOR(pgcontext.<->)",
    },
    OperatorFamilyRun {
        operator: "OPERATOR(pgcontext.<=>)",
    },
    OperatorFamilyRun {
        operator: "OPERATOR(pgcontext.<#>)",
    },
    OperatorFamilyRun {
        operator: "OPERATOR(pgcontext.<+>)",
    },
];

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "sample counts are clamped to 500, far inside every cast's exact range, and fraction is a compile-time 0.50/0.95"
)]
fn compare_percentile(sorted_ms: &[f64], fraction: f64) -> f64 {
    if sorted_ms.is_empty() {
        return 0.0;
    }
    let position = ((sorted_ms.len() - 1) as f64 * fraction).round() as usize;
    sorted_ms[position.min(sorted_ms.len() - 1)]
}

fn compare_explain_index_name(explain_json: &str) -> Option<String> {
    fn find_index_name(value: &serde_json::Value) -> Option<&str> {
        match value {
            serde_json::Value::Object(object) => object
                .get("Index Name")
                .and_then(serde_json::Value::as_str)
                .or_else(|| object.values().find_map(find_index_name)),
            serde_json::Value::Array(values) => values.iter().find_map(find_index_name),
            _ => None,
        }
    }

    let document: serde_json::Value = serde_json::from_str(explain_json).ok()?;
    find_index_name(&document).map(str::to_owned)
}

/// Measures every ANN index on one column side by side: for each operator
/// family reachable on the column, times the planner-chosen index scan over
/// sampled stored vectors and scores its top-10 recall against an exact
/// seqscan oracle using the same operator. Indexes on the column that the
/// planner never chose report NULL timings. Read-only; intended as a
/// coexist-mode diagnostic, so it is deliberately planner-driven rather
/// than reaching into either extension's internals.
#[allow(
    clippy::type_complexity,
    reason = "pgrx column naming requires the name! tuple inline in the signature"
)]
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
fn compare_indexes(
    table_name: String,
    column_name: String,
    queries: default!(i32, 20),
) -> TableIterator<
    'static,
    (
        name!(index_name, String),
        name!(access_method, String),
        name!(operator, Option<String>),
        name!(p50_ms, Option<f64>),
        name!(p95_ms, Option<f64>),
        name!(recall_at_10, Option<f64>),
    ),
> {
    let sample_count = usize::try_from(queries).unwrap_or(0).clamp(1, 500);
    // PostgreSQL's own quoting: regclass::text yields the (search_path-
    // aware) qualified, quoted relation name and quote_ident the column.
    let (table, column) = Spi::connect(|client| {
        let row = client
            .select(
                "SELECT $1::regclass::text, quote_ident($2)",
                None,
                &[table_name.clone().into(), column_name.clone().into()],
            )?
            .first();
        Ok::<_, spi::Error>((
            row.get::<String>(1)?.unwrap_or_default(),
            row.get::<String>(2)?.unwrap_or_default(),
        ))
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_TABLE,
            format!("cannot resolve {table_name}: {error}"),
        )
    });

    // Every ANN index on this column, in deterministic order.
    let indexes: Vec<(String, String)> = Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT ci.relname::text, am.amname::text
                   FROM pg_index i
                   JOIN pg_class ci ON ci.oid = i.indexrelid
                   JOIN pg_class ct ON ct.oid = i.indrelid
                   JOIN pg_am am ON am.oid = ci.relam
                   JOIN pg_attribute a ON a.attrelid = ct.oid
                                      AND a.attnum = i.indkey[0]
                  WHERE ct.oid = $1::regclass
                    AND a.attname = $2
                    AND am.amname IN (
                        'hnsw', 'ivfflat', 'pgcontext_hnsw', 'pgcontext_ivfflat'
                    )
                  ORDER BY ci.relname",
                None,
                &[table_name.clone().into(), column_name.clone().into()],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to list ANN indexes: {error}"),
                )
            });
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
            format!("failed to read ANN index inventory: {error}"),
        )
    });
    if indexes.is_empty() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
            format!("no ANN indexes found on {table_name}.{column_name}"),
        );
    }

    // Deterministic sample of stored vectors used as queries.
    let sample_sql = format!(
        "SELECT {column}::text FROM {table} WHERE {column} IS NOT NULL \
         ORDER BY ctid LIMIT {sample_count}"
    );
    let sample_vectors: Vec<String> = Spi::connect(|client| {
        let rows = client
            .select(&sample_sql, None, &[])
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to sample stored vectors: {error}"),
                )
            });
        rows.map(|row| Ok::<_, spi::Error>(row.get::<String>(1)?.unwrap_or_default()))
            .collect::<Result<Vec<_>, _>>()
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to collect sample vectors: {error}"),
        )
    });
    if sample_vectors.is_empty() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_NO_DATA_FOUND,
            format!("{table_name}.{column_name} has no non-null vectors to sample"),
        );
    }

    // (index -> measured row) for planner-chosen indexes.
    let mut measured: std::collections::BTreeMap<String, (String, f64, f64, f64)> =
        std::collections::BTreeMap::new();

    for family in COMPARE_OPERATOR_FAMILIES {
        let operator = family.operator;
        let ann_sql_for = |vector_literal: &str| {
            format!(
                "SELECT ctid::text FROM {table} \
                 ORDER BY {column} {operator} '{vector_literal}' LIMIT 10"
            )
        };
        // Probe whether this operator family applies to the column and which
        // index the planner picks for it. An inapplicable operator raises
        // through SPI (undefined function/operator), so the probe runs in a
        // PgTryBuilder subtransaction and an error simply skips the family.
        let plan_probe = format!("EXPLAIN (FORMAT JSON) {}", ann_sql_for(&sample_vectors[0]));
        Spi::run("SET LOCAL enable_seqscan = off").ok();
        let chosen = PgTryBuilder::new(|| {
            let payload = Spi::get_one::<pgrx::datum::Json>(&plan_probe)
                .ok()
                .flatten()
                .map(|json| json.0.to_string())
                .unwrap_or_default();
            compare_explain_index_name(&payload)
        })
        .catch_others(|_| None)
        .execute();
        let Some(index_name) = chosen else {
            continue;
        };
        if measured.contains_key(&index_name) {
            continue;
        }

        // Timed index-scan lane and exact same-operator oracle per query.
        // SET LOCAL must run through read-write SPI (`Spi::run`), never the
        // read-only `client.select` path, which rejects utility statements.
        let mut latencies = Vec::with_capacity(sample_vectors.len());
        let mut hits = 0_usize;
        let mut expected = 0_usize;
        for vector_literal in &sample_vectors {
            let ann_sql = ann_sql_for(vector_literal);
            Spi::run(
                "SET LOCAL enable_indexscan = on; \
                 SET LOCAL enable_bitmapscan = on; \
                 SET LOCAL enable_seqscan = off",
            )
            .ok();
            let started = std::time::Instant::now();
            let ann: Vec<String> = Spi::connect(|client| {
                client
                    .select(&ann_sql, None, &[])?
                    .map(|row| Ok::<_, spi::Error>(row.get::<String>(1)?.unwrap_or_default()))
                    .collect::<Result<Vec<_>, _>>()
            })
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("index comparison scan failed: {error}"),
                )
            });
            latencies.push(started.elapsed().as_secs_f64() * 1000.0);

            Spi::run(
                "SET LOCAL enable_indexscan = off; \
                 SET LOCAL enable_bitmapscan = off; \
                 SET LOCAL enable_seqscan = on",
            )
            .ok();
            let exact: Vec<String> = Spi::connect(|client| {
                client
                    .select(&ann_sql, None, &[])?
                    .map(|row| Ok::<_, spi::Error>(row.get::<String>(1)?.unwrap_or_default()))
                    .collect::<Result<Vec<_>, _>>()
            })
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("index comparison oracle failed: {error}"),
                )
            });
            expected += exact.len();
            hits += exact.iter().filter(|item| ann.contains(item)).count();
        }
        Spi::run(
            "SET LOCAL enable_indexscan = on; \
             SET LOCAL enable_bitmapscan = on; \
             SET LOCAL enable_seqscan = on",
        )
        .ok();
        if expected == 0 {
            continue;
        }

        latencies.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
        measured.insert(
            index_name,
            (
                operator.to_owned(),
                compare_percentile(&latencies, 0.50),
                compare_percentile(&latencies, 0.95),
                {
                    #[allow(
                        clippy::cast_precision_loss,
                        reason = "hit and oracle counts are bounded by 500 samples x 10 results"
                    )]
                    let recall = hits as f64 / expected as f64;
                    recall
                },
            ),
        );
    }

    let rows: Vec<_> = indexes
        .into_iter()
        .map(|(index_name, access_method)| {
            match measured.get(&index_name) {
                Some((operator, p50, p95, recall)) => (
                    index_name,
                    access_method,
                    Some(operator.clone()),
                    Some(*p50),
                    Some(*p95),
                    Some(*recall),
                ),
                // Present on the column but never planner-chosen for any
                // family (for example a second index shadowed by a cheaper
                // one of the same family).
                None => (index_name, access_method, None, None, None, None),
            }
        })
        .collect();
    TableIterator::new(rows)
}
