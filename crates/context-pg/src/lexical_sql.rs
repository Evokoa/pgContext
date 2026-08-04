//! SQL surface for registering, indexing, and highlighting lexical sources.
//!
//! Every function runs SECURITY INVOKER: source-table `SELECT`, RLS, and index
//! DDL privileges are enforced against the caller. Private-catalog writes are
//! routed through narrowly validated SECURITY DEFINER helpers that re-derive
//! every OID from `pg_catalog` rather than trusting caller input.

use context_query::{
    FuzzySourceName, LexicalNormalization, LexicalQuery, LexicalRankWeights, LexicalRanker,
    LexicalSourceName, LexicalWeight, MAX_LEXICAL_FIELDS, MAX_LEXICAL_HEADLINE_OPTIONS_BYTES,
    MAX_LEXICAL_HEADLINE_OUTPUT_BYTES, MAX_LEXICAL_HEADLINE_POINTS,
    MAX_LEXICAL_HEADLINE_SOURCE_BYTES, MAX_LEXICAL_JSON_PATH_DEPTH, RegisteredTsQueryName,
};
use pgrx::JsonB;
use pgrx::prelude::*;

use crate::error::{raise_query_error, raise_sql_error};
use crate::lexical_catalog::{
    LexicalIndexAm, prepare_fuzzy_source, prepare_lexical_source, require_collection_owner_id,
};
use crate::table_search::{quote_identifier, quote_qualified_identifier};

const DEFAULT_TEXT_CONFIGURATION: &str = "pg_catalog.english";

/// Registers a lexical source built from ordered weighted text or JSON fields.
///
/// `text_columns` names source-table columns in document order. `field_weights`
/// assigns each column a PostgreSQL `A`..`D` weight (default `D`), and
/// `json_paths` optionally supplies a dotted path into a `json`/`jsonb` column.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
#[allow(
    clippy::too_many_arguments,
    reason = "the registration contract is one flat SQL signature by design"
)]
pub fn register_lexical_source(
    collection: String,
    source_name: String,
    text_columns: Vec<Option<String>>,
    text_configuration: default!(String, "'pg_catalog.english'"),
    field_weights: default!(Option<Vec<Option<String>>>, "NULL"),
    json_paths: default!(Option<Vec<Option<String>>>, "NULL"),
    ranker: default!(String, "'ts_rank_cd'"),
    normalization: default!(i32, 0),
    rank_weights: default!(Option<Vec<Option<f32>>>, "NULL"),
) -> i64 {
    let collection_id = require_collection_owner_id(&collection);
    let source_name = validated_lexical_name(source_name);
    let columns = required_columns(text_columns);
    let weights = resolve_field_weights(field_weights.as_ref(), columns.len());
    let paths = resolve_json_paths(json_paths.as_ref(), columns.len());
    let (configuration_schema, configuration_name) = split_configuration(&text_configuration);
    let ranker = LexicalRanker::parse(&ranker).unwrap_or_else(|error| raise_query_error(error));
    let normalization = validated_normalization(normalization);
    let rank_weights = resolve_rank_weights(rank_weights.as_ref());

    register_source(LexicalRegistration {
        collection_id,
        source_name: &source_name,
        document_mode: "fields",
        document_column: None,
        configuration_schema,
        configuration_name,
        ranker,
        normalization,
        rank_weights,
        columns,
        weights,
        paths,
    })
}

/// Registers a lexical source backed by a stored or generated `tsvector` column.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn register_lexical_document_source(
    collection: String,
    source_name: String,
    document_column: String,
    text_configuration: default!(String, "'pg_catalog.english'"),
    ranker: default!(String, "'ts_rank_cd'"),
    normalization: default!(i32, 0),
    rank_weights: default!(Option<Vec<Option<f32>>>, "NULL"),
) -> i64 {
    let collection_id = require_collection_owner_id(&collection);
    let source_name = validated_lexical_name(source_name);
    let (configuration_schema, configuration_name) = split_configuration(&text_configuration);
    let ranker = LexicalRanker::parse(&ranker).unwrap_or_else(|error| raise_query_error(error));
    let normalization = validated_normalization(normalization);
    let rank_weights = resolve_rank_weights(rank_weights.as_ref());

    register_source(LexicalRegistration {
        collection_id,
        source_name: &source_name,
        document_mode: "stored_vector",
        document_column: Some(&document_column),
        configuration_schema,
        configuration_name,
        ranker,
        normalization,
        rank_weights,
        columns: Vec::new(),
        weights: Vec::new(),
        paths: Vec::new(),
    })
}

/// Binds a per-row `tsquery` column to a registered lexical source.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn register_lexical_tsquery(
    collection: String,
    source_name: String,
    tsquery_name: String,
    tsquery_column: String,
) -> bool {
    let collection_id = require_collection_owner_id(&collection);
    let source_name = validated_lexical_name(source_name);
    let tsquery_name =
        RegisteredTsQueryName::new(tsquery_name).unwrap_or_else(|error| raise_query_error(error));
    run_definer(
        "SELECT pgcontext._register_lexical_tsquery($1, $2, $3, $4)",
        &[
            collection_id.into(),
            source_name.as_str().into(),
            tsquery_name.as_str().into(),
            tsquery_column.as_str().into(),
        ],
    );
    true
}

/// Creates and attaches the canonical GIN or GiST index for a lexical source.
///
/// The index expression is rendered from the same canonical document SQL the
/// query paths use, so the planner can match it structurally.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn create_lexical_index(
    collection: String,
    source_name: String,
    method: default!(String, "'gin'"),
) -> String {
    let collection_id = require_collection_owner_id(&collection);
    let source_name = validated_lexical_name(source_name);
    let access_method = parse_access_method(&method);
    let prepared = prepare_lexical_source(collection_id, source_name.as_str())
        .unwrap_or_else(|error| raise_query_error(error));
    let index_name = index_name_for("lexical", collection_id, source_name.as_str());
    let statement = format!(
        "CREATE INDEX {index} ON {table} USING {method} ({key})",
        index = quote_identifier(&index_name),
        table = quote_qualified_identifier(&prepared.schema_name, &prepared.table_name),
        method = access_method.stable_name(),
        key = prepared.index_key_sql(access_method),
    );
    run_ddl(&statement);
    attach_index(
        "SELECT pgcontext._attach_lexical_index($1, $2, $3, $4)",
        collection_id,
        source_name.as_str(),
        &prepared.schema_name,
        &index_name,
    );
    index_name
}

/// Attaches an existing index to a registered lexical source.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn attach_lexical_index(collection: String, source_name: String, index_name: String) -> bool {
    let collection_id = require_collection_owner_id(&collection);
    let source_name = validated_lexical_name(source_name);
    let prepared = prepare_lexical_source(collection_id, source_name.as_str())
        .unwrap_or_else(|error| raise_query_error(error));
    attach_index(
        "SELECT pgcontext._attach_lexical_index($1, $2, $3, $4)",
        collection_id,
        source_name.as_str(),
        &prepared.schema_name,
        &index_name,
    );
    true
}

/// Detaches the attached index from a registered lexical source.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn detach_lexical_index(collection: String, source_name: String) -> bool {
    let collection_id = require_collection_owner_id(&collection);
    let source_name = validated_lexical_name(source_name);
    run_definer(
        "SELECT pgcontext._detach_lexical_index($1, $2)",
        &[collection_id.into(), source_name.as_str().into()],
    );
    true
}

/// Drops a registered lexical source and its field bindings.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn drop_lexical_source(collection: String, source_name: String) -> bool {
    let collection_id = require_collection_owner_id(&collection);
    let source_name = validated_lexical_name(source_name);
    Spi::get_one_with_args::<bool>(
        "SELECT pgcontext._drop_lexical_source($1, $2)",
        &[collection_id.into(), source_name.as_str().into()],
    )
    .unwrap_or_else(|error| raise_definer_error(&error.to_string()))
    .unwrap_or(false)
}

/// Registers a `pg_trgm` fuzzy source over one source-table text column.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn register_fuzzy_source(collection: String, source_name: String, text_column: String) -> i64 {
    let collection_id = require_collection_owner_id(&collection);
    let source_name =
        FuzzySourceName::new(source_name).unwrap_or_else(|error| raise_query_error(error));
    Spi::get_one_with_args::<i64>(
        "SELECT pgcontext._register_fuzzy_source($1, $2, $3)",
        &[
            collection_id.into(),
            source_name.as_str().into(),
            text_column.as_str().into(),
        ],
    )
    .unwrap_or_else(|error| raise_definer_error(&error.to_string()))
    .unwrap_or_else(|| raise_definer_error("fuzzy source registration returned null"))
}

/// Creates and attaches the canonical trigram index for a fuzzy source.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn create_fuzzy_index(
    collection: String,
    source_name: String,
    method: default!(String, "'gin'"),
) -> String {
    let collection_id = require_collection_owner_id(&collection);
    let source_name =
        FuzzySourceName::new(source_name).unwrap_or_else(|error| raise_query_error(error));
    let access_method = parse_access_method(&method);
    let prepared = prepare_fuzzy_source(collection_id, source_name.as_str())
        .unwrap_or_else(|error| raise_query_error(error));
    let index_name = index_name_for("fuzzy", collection_id, source_name.as_str());
    let statement = format!(
        "CREATE INDEX {index} ON {table} USING {method} (({text}) {operator_class})",
        index = quote_identifier(&index_name),
        table = quote_qualified_identifier(&prepared.schema_name, &prepared.table_name),
        method = access_method.stable_name(),
        text = prepared.text_sql(None),
        operator_class = prepared.operator_class(access_method),
    );
    run_ddl(&statement);
    attach_index(
        "SELECT pgcontext._attach_fuzzy_index($1, $2, $3, $4)",
        collection_id,
        source_name.as_str(),
        &prepared.schema_name,
        &index_name,
    );
    index_name
}

/// Attaches an existing trigram index to a registered fuzzy source.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn attach_fuzzy_index(collection: String, source_name: String, index_name: String) -> bool {
    let collection_id = require_collection_owner_id(&collection);
    let source_name =
        FuzzySourceName::new(source_name).unwrap_or_else(|error| raise_query_error(error));
    let prepared = prepare_fuzzy_source(collection_id, source_name.as_str())
        .unwrap_or_else(|error| raise_query_error(error));
    attach_index(
        "SELECT pgcontext._attach_fuzzy_index($1, $2, $3, $4)",
        collection_id,
        source_name.as_str(),
        &prepared.schema_name,
        &index_name,
    );
    true
}

/// Detaches the attached index from a registered fuzzy source.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn detach_fuzzy_index(collection: String, source_name: String) -> bool {
    let collection_id = require_collection_owner_id(&collection);
    let source_name =
        FuzzySourceName::new(source_name).unwrap_or_else(|error| raise_query_error(error));
    run_definer(
        "SELECT pgcontext._detach_fuzzy_index($1, $2)",
        &[collection_id.into(), source_name.as_str().into()],
    );
    true
}

/// Drops a registered fuzzy source.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn drop_fuzzy_source(collection: String, source_name: String) -> bool {
    let collection_id = require_collection_owner_id(&collection);
    let source_name =
        FuzzySourceName::new(source_name).unwrap_or_else(|error| raise_query_error(error));
    Spi::get_one_with_args::<bool>(
        "SELECT pgcontext._drop_fuzzy_source($1, $2)",
        &[collection_id.into(), source_name.as_str().into()],
    )
    .unwrap_or_else(|error| raise_definer_error(&error.to_string()))
    .unwrap_or(false)
}

/// Re-derives lexical and fuzzy catalog OIDs from their stable names.
///
/// Run this after a dump/restore or a source-table rewrite. Rows whose stable
/// names no longer resolve to a compatible object are left untouched and fail
/// closed at query time.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn refresh_lexical_catalog(collection: String) -> i64 {
    let collection_id = require_collection_owner_id(&collection);
    Spi::get_one_with_args::<i64>(
        "SELECT pgcontext._refresh_lexical_catalog_oids($1)",
        &[collection_id.into()],
    )
    .unwrap_or_else(|error| raise_definer_error(&error.to_string()))
    .unwrap_or_default()
}

/// Lists registered lexical sources visible to the caller.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx requires inline name!() table shapes for SQL generation"
)]
pub fn lexical_sources(
    collection: String,
) -> TableIterator<
    'static,
    (
        name!(source_name, String),
        name!(document_mode, String),
        name!(text_configuration, String),
        name!(ranker, String),
        name!(normalization, i32),
        name!(index_name, Option<String>),
        name!(index_method, Option<String>),
        name!(registration_revision, i64),
        name!(status, String),
    ),
> {
    let collection_id = require_collection_owner_id(&collection);
    let rows = Spi::connect(|client| {
        let spi_rows = client
            .select(
                "SELECT source_name,
                        document_mode,
                        configuration_schema_name || '.' || configuration_name,
                        ranker,
                        normalization,
                        index_name,
                        index_am_name,
                        registration_revision,
                        status
                   FROM pgcontext._visible_collection_lexical_sources
                  WHERE collection_id = $1
                  ORDER BY source_name",
                None,
                &[collection_id.into()],
            )
            .unwrap_or_else(|error| raise_definer_error(&error.to_string()));
        let mut rows = Vec::new();
        for row in spi_rows {
            rows.push((
                text_column(&row, 1),
                text_column(&row, 2),
                text_column(&row, 3),
                text_column(&row, 4),
                row.get::<i32>(5).ok().flatten().unwrap_or_default(),
                row.get::<String>(6).ok().flatten(),
                row.get::<String>(7).ok().flatten(),
                row.get::<i64>(8).ok().flatten().unwrap_or_default(),
                text_column(&row, 9),
            ));
        }
        rows
    });
    TableIterator::new(rows)
}

/// Lists registered fuzzy sources visible to the caller.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx requires inline name!() table shapes for SQL generation"
)]
pub fn fuzzy_sources(
    collection: String,
) -> TableIterator<
    'static,
    (
        name!(source_name, String),
        name!(text_column, String),
        name!(trgm_schema, String),
        name!(index_name, Option<String>),
        name!(index_method, Option<String>),
        name!(registration_revision, i64),
        name!(status, String),
    ),
> {
    let collection_id = require_collection_owner_id(&collection);
    let rows = Spi::connect(|client| {
        let spi_rows = client
            .select(
                "SELECT source_name,
                        text_column_name,
                        trgm_schema_name,
                        index_name,
                        index_am_name,
                        registration_revision,
                        status
                   FROM pgcontext._visible_collection_fuzzy_sources
                  WHERE collection_id = $1
                  ORDER BY source_name",
                None,
                &[collection_id.into()],
            )
            .unwrap_or_else(|error| raise_definer_error(&error.to_string()));
        let mut rows = Vec::new();
        for row in spi_rows {
            rows.push((
                text_column(&row, 1),
                text_column(&row, 2),
                text_column(&row, 3),
                row.get::<String>(4).ok().flatten(),
                row.get::<String>(5).ok().flatten(),
                row.get::<i64>(6).ok().flatten().unwrap_or_default(),
                text_column(&row, 7),
            ));
        }
        rows
    });
    TableIterator::new(rows)
}

/// Returns bounded `ts_headline` fragments for already-retrieved points.
///
/// Point count, source-document bytes, option bytes, and total output bytes are
/// each admitted before PostgreSQL builds any markup. The returned text is
/// PostgreSQL's own `ts_headline` output and carries `<b>`/`</b>` markup by
/// default: callers **must** sanitize it for their own output context before
/// rendering it in HTML or any other markup-sensitive surface.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx requires inline name!() table shapes for SQL generation"
)]
pub fn lexical_headline(
    collection: String,
    source_name: String,
    point_ids: Vec<Option<i64>>,
    query: JsonB,
    options: default!(String, "''"),
) -> TableIterator<'static, (name!(point_id, i64), name!(headline, String))> {
    let collection_id = require_collection_owner_id(&collection);
    let source_name = validated_lexical_name(source_name);
    let lexical =
        LexicalQuery::from_json(&query.0).unwrap_or_else(|error| raise_query_error(error));
    let prepared = prepare_lexical_source(collection_id, source_name.as_str())
        .unwrap_or_else(|error| raise_query_error(error));
    if options.len() > MAX_LEXICAL_HEADLINE_OPTIONS_BYTES {
        raise_bounds("headline options exceed the admitted byte budget");
    }
    let requested = point_ids.into_iter().flatten().collect::<Vec<_>>();
    if requested.is_empty() || requested.len() > MAX_LEXICAL_HEADLINE_POINTS {
        raise_bounds("headline point count is empty or exceeds the admitted maximum");
    }
    let rows = crate::retrieval::lexical_headline_rows(
        &prepared,
        &lexical,
        &requested,
        &options,
        MAX_LEXICAL_HEADLINE_SOURCE_BYTES,
        MAX_LEXICAL_HEADLINE_OUTPUT_BYTES,
    )
    .unwrap_or_else(|error| raise_query_error(error));
    TableIterator::new(rows)
}

/// Validated registration request handed to the SECURITY DEFINER catalog write.
struct LexicalRegistration<'a> {
    collection_id: i64,
    source_name: &'a LexicalSourceName,
    document_mode: &'a str,
    document_column: Option<&'a str>,
    configuration_schema: String,
    configuration_name: String,
    ranker: LexicalRanker,
    normalization: LexicalNormalization,
    rank_weights: LexicalRankWeights,
    columns: Vec<String>,
    weights: Vec<String>,
    paths: Vec<Option<String>>,
}

fn register_source(registration: LexicalRegistration<'_>) -> i64 {
    let LexicalRegistration {
        collection_id,
        source_name,
        document_mode,
        document_column,
        configuration_schema,
        configuration_name,
        ranker,
        normalization,
        rank_weights,
        columns,
        weights,
        paths,
    } = registration;
    let weight_array = weights;
    let path_array = paths;
    let column_array = columns;
    let rank_array = rank_weights.as_array().to_vec();
    let normalization = i32::try_from(normalization.get())
        .unwrap_or_else(|_| raise_bounds("normalization exceeds the admitted range"));
    Spi::get_one_with_args::<i64>(
        "SELECT pgcontext._register_lexical_source($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        &[
            collection_id.into(),
            source_name.as_str().into(),
            document_mode.into(),
            document_column.into(),
            configuration_schema.as_str().into(),
            configuration_name.as_str().into(),
            ranker.stable_name().into(),
            normalization.into(),
            rank_array.into(),
            column_array.into(),
            weight_array.into(),
            path_array.into(),
        ],
    )
    .unwrap_or_else(|error| raise_definer_error(&error.to_string()))
    .unwrap_or_else(|| raise_definer_error("lexical source registration returned null"))
}

fn attach_index(
    statement: &str,
    collection_id: i64,
    source_name: &str,
    index_schema: &str,
    index_name: &str,
) {
    run_definer(
        statement,
        &[
            collection_id.into(),
            source_name.into(),
            index_schema.into(),
            index_name.into(),
        ],
    );
}

fn run_definer(statement: &str, args: &[pgrx::datum::DatumWithOid<'_>]) {
    Spi::connect_mut(|client| {
        client
            .update(statement, Some(1), args)
            .map(|_| ())
            .unwrap_or_else(|error| raise_definer_error(&error.to_string()));
    });
}

fn run_ddl(statement: &str) {
    Spi::run(statement).unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to create lexical index: {error}"),
        )
    });
}

/// Maximum bytes PostgreSQL accepts in a relation name.
const MAX_INDEX_NAME_BYTES: usize = 63;

/// Returns a bounded, collision-resistant index name.
///
/// Truncating a long source name alone could map two distinct registrations to
/// the same relation name and surface as a raw `relation already exists`, so a
/// truncated name carries a stable digest of the full identity instead.
fn index_name_for(prefix: &str, collection_id: i64, source_name: &str) -> String {
    let candidate = format!("pgcontext_{prefix}_{collection_id}_{source_name}");
    if candidate.len() <= MAX_INDEX_NAME_BYTES {
        return candidate;
    }
    let digest = format!("{:016x}", fnv1a(candidate.as_bytes()));
    let head = MAX_INDEX_NAME_BYTES.saturating_sub(digest.len().saturating_add(1));
    let mut truncated = candidate.chars().take(head).collect::<String>();
    truncated.push('_');
    truncated.push_str(&digest);
    truncated
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn parse_access_method(method: &str) -> LexicalIndexAm {
    match method {
        "gin" => LexicalIndexAm::Gin,
        "gist" => LexicalIndexAm::Gist,
        _ => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "lexical index method must be gin or gist".to_owned(),
        ),
    }
}

fn validated_lexical_name(source_name: String) -> LexicalSourceName {
    LexicalSourceName::new(source_name).unwrap_or_else(|error| raise_query_error(error))
}

fn validated_normalization(normalization: i32) -> LexicalNormalization {
    let mask = u32::try_from(normalization)
        .unwrap_or_else(|_| raise_bounds("normalization must be non-negative"));
    LexicalNormalization::new(mask).unwrap_or_else(|error| raise_query_error(error))
}

fn required_columns(text_columns: Vec<Option<String>>) -> Vec<String> {
    let columns = text_columns.into_iter().flatten().collect::<Vec<_>>();
    if columns.is_empty() || columns.len() > MAX_LEXICAL_FIELDS {
        raise_bounds("lexical sources require 1..=16 non-null text columns");
    }
    columns
}

fn resolve_field_weights(weights: Option<&Vec<Option<String>>>, columns: usize) -> Vec<String> {
    let Some(weights) = weights else {
        return vec![LexicalWeight::D.stable_name().to_owned(); columns];
    };
    if weights.len() != columns {
        raise_bounds("field weights must match the text column count");
    }
    weights
        .iter()
        .map(|weight| {
            let weight = weight
                .as_deref()
                .unwrap_or_else(|| LexicalWeight::D.stable_name());
            LexicalWeight::parse(&weight.to_lowercase())
                .unwrap_or_else(|error| raise_query_error(error))
                .stable_name()
                .to_owned()
        })
        .collect()
}

fn resolve_json_paths(paths: Option<&Vec<Option<String>>>, columns: usize) -> Vec<Option<String>> {
    let Some(paths) = paths else {
        return vec![None; columns];
    };
    if paths.len() != columns {
        raise_bounds("JSON paths must match the text column count");
    }
    paths
        .iter()
        .map(|path| {
            let path = path.as_deref().filter(|path| !path.is_empty())?;
            let depth = path.split('.').count();
            if depth == 0 || depth > MAX_LEXICAL_JSON_PATH_DEPTH {
                raise_bounds("JSON path depth exceeds the admitted maximum");
            }
            if path.split('.').any(str::is_empty) {
                raise_bounds("JSON path components must not be empty");
            }
            Some(path.to_owned())
        })
        .collect()
}

fn resolve_rank_weights(rank_weights: Option<&Vec<Option<f32>>>) -> LexicalRankWeights {
    let Some(weights) = rank_weights else {
        return LexicalRankWeights::DEFAULT;
    };
    if weights.len() != 4 {
        raise_bounds("rank weights must contain exactly four {D,C,B,A} values");
    }
    let values = weights
        .iter()
        .map(|weight| weight.unwrap_or_else(|| raise_bounds("rank weights must not be null")))
        .collect::<Vec<_>>();
    LexicalRankWeights::new(values[0], values[1], values[2], values[3])
        .unwrap_or_else(|error| raise_query_error(error))
}

fn split_configuration(configuration: &str) -> (String, String) {
    match configuration.split_once('.') {
        Some((schema, name)) if !schema.is_empty() && !name.is_empty() => {
            (schema.to_owned(), name.to_owned())
        }
        None if !configuration.is_empty() => ("pg_catalog".to_owned(), configuration.to_owned()),
        _ => {
            let (schema, name) = DEFAULT_TEXT_CONFIGURATION
                .split_once('.')
                .unwrap_or(("pg_catalog", "english"));
            (schema.to_owned(), name.to_owned())
        }
    }
}

fn text_column(row: &spi::SpiHeapTupleData<'_>, index: usize) -> String {
    row.get::<String>(index)
        .ok()
        .flatten()
        .unwrap_or_else(|| raise_definer_error("lexical catalog column is null"))
}

fn raise_bounds(message: &str) -> ! {
    raise_sql_error(
        PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
        message.to_owned(),
    )
}

fn raise_definer_error(message: &str) -> ! {
    raise_sql_error(
        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
        format!("lexical catalog write failed: {message}"),
    )
}
