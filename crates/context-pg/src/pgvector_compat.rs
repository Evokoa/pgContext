//! pgvector coexist-mode surface: migration inventory, index adoption,
//! and the once-per-backend advisory nudge.
//!
//! Coexist mode (pgvector installed first, pgContext second) serves
//! pgvector-typed columns natively: the SQL artifact skips creating our
//! colliding type/cast/typmod objects and every pgContext opclass and
//! function binds to pgvector's identically-laid-out types by name
//! resolution. Everything here is guidance and tooling on top of that —
//! results over pgvector columns are always complete; nothing in this
//! module gates or degrades a query.

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
    Option<i32>,
    Vec<String>,
    Vec<String>,
    String,
);

/// Returns whether `type_oid` belongs to the pgvector extension.
fn type_owned_by_pgvector(type_oid: pg_sys::Oid) -> bool {
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
        "vector_l2_ops" => Some("pgcontext.vector_hnsw_ops"),
        "vector_cosine_ops" => Some("pgcontext.vector_hnsw_cosine_ops"),
        "vector_ip_ops" => Some("pgcontext.vector_hnsw_ip_ops"),
        "vector_l1_ops" => Some("pgcontext.vector_hnsw_l1_ops"),
        "halfvec_l2_ops" => Some("pgcontext.halfvec_hnsw_ops"),
        "sparsevec_l2_ops" => Some("pgcontext.sparsevec_hnsw_ops"),
        _ => None,
    }
}

const MIGRATION_REPORT_SQL: &str = r"
WITH vector_columns AS (
    SELECT c.oid AS table_oid,
           n.nspname AS schema_name,
           c.relname AS table_name,
           a.attnum,
           a.attname AS column_name,
           CASE WHEN a.atttypmod > 0 THEN a.atttypmod ELSE NULL END AS dimensions
      FROM pg_attribute a
      JOIN pg_class c ON c.oid = a.attrelid
      JOIN pg_namespace n ON n.oid = c.relnamespace
      JOIN pg_type t ON t.oid = a.atttypid
      JOIN pg_depend d ON d.classid = 'pg_type'::regclass
                      AND d.objid = t.oid AND d.deptype = 'e'
      JOIN pg_extension e ON e.oid = d.refobjid AND e.extname = 'vector'
     WHERE c.relkind IN ('r', 'p', 'm')
       AND a.attnum > 0
       AND NOT a.attisdropped
       AND n.nspname NOT IN ('pg_catalog', 'information_schema')
)
SELECT vc.schema_name::text,
       vc.table_name::text,
       vc.column_name::text,
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
         LIMIT 1) AS first_pgvector_opclass
  FROM vector_columns vc
 ORDER BY 1, 2, 3";

/// Read-only inventory of pgvector-typed columns and how to migrate their
/// indexing to pgContext. Safe to run anywhere; touches only catalogs.
#[allow(
    clippy::type_complexity,
    reason = "pgrx column naming requires the name! tuple inline in the signature"
)]
#[pg_extern(schema = "pgcontext")]
#[search_path(pg_catalog, pgcontext, public)]
fn migration_report() -> TableIterator<
    'static,
    (
        name!(schema_name, String),
        name!(table_name, String),
        name!(column_name, String),
        name!(dimensions, Option<i32>),
        name!(pgvector_indexes, Vec<String>),
        name!(pgcontext_indexes, Vec<String>),
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
            let dimensions: Option<i32> = row.get(4).unwrap_or(None);
            let pgvector_indexes: Vec<String> = row.get(5).unwrap_or(None).unwrap_or_default();
            let pgcontext_indexes: Vec<String> = row.get(6).unwrap_or(None).unwrap_or_default();
            let first_opclass: Option<String> = row.get(7).unwrap_or(None);
            let suggested = suggest_command(
                &schema,
                &table_name,
                &column,
                first_opclass.as_deref(),
                !pgcontext_indexes.is_empty(),
            );
            rows.push((
                schema,
                table_name,
                column,
                dimensions,
                pgvector_indexes,
                pgcontext_indexes,
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
    first_opclass: Option<&str>,
    has_pgcontext_index: bool,
) -> String {
    if has_pgcontext_index {
        return "already indexed by pgcontext_hnsw".to_owned();
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
        None => "pgcontext.vector_hnsw_cosine_ops",
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
       a.attname::text AS column_name,
       o.opcname::text AS opclass_name,
       am.amname::text AS am_name
  FROM pg_index i
  JOIN pg_class ci ON ci.oid = i.indexrelid
  JOIN pg_class ct ON ct.oid = i.indrelid
  JOIN pg_namespace n ON n.oid = ct.relnamespace
  JOIN pg_am am ON am.oid = ci.relam
  JOIN pg_opclass o ON o.oid = i.indclass[0]
  LEFT JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = i.indkey[0]
 WHERE am.amname IN ('hnsw', 'ivfflat')
   AND ($1::oid IS NULL OR i.indrelid = $1::oid)
 ORDER BY 1, 2, 3";

/// Migrates pgvector `hnsw`/`ivfflat` indexes to `pgcontext_hnsw`
/// equivalents. `dry_run` (the default) only reports the commands;
/// `drop_old` additionally drops each pgvector index after its
/// replacement builds. Column types are untouched — in coexist mode the
/// data stays in pgvector's types by design.
#[pg_extern(schema = "pgcontext")]
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
    let target_oid: Option<pg_sys::Oid> = target.as_ref().map(|relation| relation.oid());
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
            let column: Option<String> = row.get(4).unwrap_or(None);
            let opclass: String = spi_required(row.get(5), "opclass_name");
            plans.push((schema, table_name, index, column, opclass));
        }
        plans
    });

    let mut rows = Vec::new();
    for (schema, table_name, index, column, opclass) in plans {
        let Some(column) = column else {
            rows.push((
                index,
                "skipped: expression index (no plain indexed column)".to_owned(),
                String::new(),
                false,
            ));
            continue;
        };
        let Some(mapped) = map_pgvector_opclass(&opclass) else {
            rows.push((
                index,
                format!("skipped: no pgContext equivalent for opclass {opclass}"),
                String::new(),
                false,
            ));
            continue;
        };
        let new_index = format!("{}_pgc", &index[..index.len().min(59)]);
        let create = format!(
            "CREATE INDEX {} ON {}.{} USING pgcontext_hnsw ({} {})",
            quote_ident(&new_index),
            quote_ident(&schema),
            quote_ident(&table_name),
            quote_ident(&column),
            mapped,
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

/// Explains how to reach coexist mode when pgvector was installed AFTER
/// pgContext (our types already occupy the public names, so a live
/// rebind is not possible).
#[pg_extern(schema = "pgcontext")]
#[search_path(pg_catalog, pgcontext, public)]
fn enable_pgvector_binding() {
    raise_sql_error(
        PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
        "pgContext was installed before pgvector, so pgContext's own vector \
         types occupy the public type names and cannot be rebound in place. \
         To run in coexist mode, install pgvector first and pgContext second \
         (DROP EXTENSION pgcontext, CREATE EXTENSION vector, CREATE EXTENSION \
         pgcontext). pgContext indexes and collections must then be recreated.",
    )
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

/// Extracts `"Index Name": "..."` from an `EXPLAIN (FORMAT JSON)` payload
/// without a JSON dependency; index names produced by this comparison flow
/// are ordinary identifiers with no embedded quotes.
fn compare_explain_index_name(explain_json: &str) -> Option<String> {
    let key = "\"Index Name\"";
    let after_key = explain_json.find(key)? + key.len();
    let rest = &explain_json[after_key..];
    let colon = rest.find(':')?;
    let rest = &rest[colon + 1..];
    let open_quote = rest.find('"')?;
    let rest = &rest[open_quote + 1..];
    let close_quote = rest.find('"')?;
    Some(rest[..close_quote].to_owned())
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
#[pg_extern(schema = "pgcontext")]
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
                    AND am.amname IN ('hnsw', 'ivfflat', 'pgcontext_hnsw')
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
