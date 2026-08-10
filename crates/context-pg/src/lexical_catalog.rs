//! Registration, validation, and preparation for PostgreSQL-native lexical sources.
//!
//! `context-query` owns lexical semantics; this module owns every PostgreSQL
//! identity concern: regconfig and column OIDs, catalog rows, index validation,
//! ACL and membership checks, and the canonical document SQL that both index
//! creation and query execution render from.

use std::collections::BTreeSet;

use context_core::CollectionName;
use context_query::{
    LexicalNormalization, LexicalRankWeights, LexicalRanker, LexicalWeight, MAX_LEXICAL_FIELDS,
    MAX_LEXICAL_JSON_PATH_DEPTH, QueryError, Result as QueryResult,
};
use pgrx::prelude::*;

use crate::error::raise_sql_error;
use crate::table_search::{quote_identifier, quote_qualified_identifier, resolve_collection};

/// Access method backing an attached lexical or fuzzy index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LexicalIndexAm {
    /// PostgreSQL `gin`.
    Gin,
    /// PostgreSQL `gist`.
    Gist,
}

impl LexicalIndexAm {
    pub(crate) const fn stable_name(self) -> &'static str {
        match self {
            Self::Gin => "gin",
            Self::Gist => "gist",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        match name {
            "gin" => Some(Self::Gin),
            "gist" => Some(Self::Gist),
            _ => None,
        }
    }
}

/// One ordered registered lexical field binding.
#[derive(Clone, Debug)]
pub(crate) struct LexicalFieldBinding {
    pub(crate) column_name: String,
    pub(crate) attnum: i16,
    pub(crate) type_oid: pg_sys::Oid,
    pub(crate) collation_oid: pg_sys::Oid,
    pub(crate) json_path: Option<Vec<String>>,
    pub(crate) weight: LexicalWeight,
}

/// How a registered lexical source produces its `tsvector` document.
#[derive(Clone, Debug)]
pub(crate) enum LexicalDocument {
    /// Weighted concatenation of raw text or JSON-path fields.
    Fields(Vec<LexicalFieldBinding>),
    /// A stored, generated, or trigger-maintained `tsvector` column.
    StoredVector { column_name: String, attnum: i16 },
}

/// Validated attached index identity.
#[derive(Clone, Debug)]
pub(crate) struct LexicalIndexBinding {
    pub(crate) index_oid: pg_sys::Oid,
    pub(crate) index_name: String,
    pub(crate) access_method: LexicalIndexAm,
    pub(crate) definition: String,
    pub(crate) is_lossy: bool,
}

/// Fully validated lexical source ready to render SQL.
#[derive(Clone, Debug)]
pub(crate) struct PreparedLexicalSource {
    pub(crate) lexical_source_id: i64,
    pub(crate) collection_id: i64,
    pub(crate) source_name: String,
    pub(crate) schema_name: String,
    pub(crate) table_name: String,
    pub(crate) table_oid: pg_sys::Oid,
    pub(crate) configuration_schema_name: String,
    pub(crate) configuration_name: String,
    pub(crate) document: LexicalDocument,
    pub(crate) ranker: LexicalRanker,
    pub(crate) normalization: LexicalNormalization,
    pub(crate) rank_weights: LexicalRankWeights,
    pub(crate) row_tsquery: Option<(String, String)>,
    pub(crate) index: Option<LexicalIndexBinding>,
    pub(crate) registration_revision: i64,
    rendered: RenderedLexicalSql,
}

/// SQL fragments rendered once per prepared source.
///
/// Identifier and literal quoting round-trips through PostgreSQL `format()`, so
/// rendering the document per statement would issue dozens of SPI calls on the
/// query hot path. Every fragment is derived only from validated registration
/// metadata, so it is safe to reuse for the lifetime of the prepared source.
#[derive(Clone, Debug, Default)]
struct RenderedLexicalSql {
    qualified_table: String,
    configuration: String,
    document_aliased: String,
    document_bare: String,
    headline_text_aliased: Option<String>,
}

/// Fully validated `pg_trgm` source ready to render SQL.
#[derive(Clone, Debug)]
pub(crate) struct PreparedFuzzySource {
    pub(crate) collection_id: i64,
    pub(crate) schema_name: String,
    pub(crate) table_name: String,
    pub(crate) table_oid: pg_sys::Oid,
    pub(crate) text_column_name: String,
    pub(crate) text_attnum: i16,
    pub(crate) text_type_oid: pg_sys::Oid,
    pub(crate) text_collation_oid: pg_sys::Oid,
    pub(crate) trgm_extension_oid: pg_sys::Oid,
    pub(crate) trgm_schema_name: String,
    pub(crate) index: Option<LexicalIndexBinding>,
    pub(crate) registration_revision: i64,
}

impl PreparedLexicalSource {
    /// Renders and memoizes every SQL fragment derived from registration metadata.
    fn render(&mut self) {
        let qualified_table = quote_qualified_identifier(&self.schema_name, &self.table_name);
        let configuration = format!(
            "{}::pg_catalog.regconfig",
            quote_literal(&quote_qualified_identifier(
                &self.configuration_schema_name,
                &self.configuration_name,
            ))
        );
        self.rendered = RenderedLexicalSql {
            document_aliased: render_document(&self.document, &configuration, Some(SOURCE_ALIAS)),
            document_bare: render_document(&self.document, &configuration, None),
            headline_text_aliased: render_headline_text(&self.document, Some(SOURCE_ALIAS)),
            qualified_table,
            configuration,
        };
    }

    /// Returns the schema-qualified source relation.
    pub(crate) fn qualified_table(&self) -> &str {
        &self.rendered.qualified_table
    }

    /// Returns the immutable `regconfig` literal used by every rendered path.
    pub(crate) fn configuration_sql(&self) -> &str {
        &self.rendered.configuration
    }

    /// Returns the canonical document expression qualified by the source alias.
    ///
    /// The same rendering backs index creation and every query path, so a
    /// registered index is structurally matchable by the planner.
    pub(crate) fn document_sql(&self) -> &str {
        &self.rendered.document_aliased
    }

    /// Returns the concatenated raw text used by bounded headline hydration.
    ///
    /// Stored-vector sources carry no raw text, so they return `None` and the
    /// headline path fails closed rather than highlighting a lossy document.
    pub(crate) fn headline_text_sql(&self) -> Option<&str> {
        self.rendered.headline_text_aliased.as_deref()
    }

    /// Returns the `CREATE INDEX` key expression for this source.
    pub(crate) fn index_key_sql(&self, access_method: LexicalIndexAm) -> String {
        let document = &self.rendered.document_bare;
        match access_method {
            LexicalIndexAm::Gin => format!("({document})"),
            LexicalIndexAm::Gist => format!("({document}) pg_catalog.tsvector_ops"),
        }
    }
}

/// SQL alias bound to the registered source relation in every rendered statement.
pub(crate) const SOURCE_ALIAS: &str = "source";

fn render_document(document: &LexicalDocument, configuration: &str, alias: Option<&str>) -> String {
    match document {
        LexicalDocument::StoredVector { column_name, .. } => column_sql(column_name, alias),
        LexicalDocument::Fields(fields) => fields
            .iter()
            .map(|field| {
                format!(
                    "pg_catalog.setweight(pg_catalog.to_tsvector({configuration}, {text}), {weight}::\"char\")",
                    text = field_text_sql(field, alias),
                    weight = quote_literal(&field.weight.label().to_string()),
                )
            })
            .collect::<Vec<_>>()
            .join(" OPERATOR(pg_catalog.||) "),
    }
}

fn render_headline_text(document: &LexicalDocument, alias: Option<&str>) -> Option<String> {
    match document {
        LexicalDocument::StoredVector { .. } => None,
        LexicalDocument::Fields(fields) => Some(
            fields
                .iter()
                .map(|field| field_text_sql(field, alias))
                .collect::<Vec<_>>()
                .join(" OPERATOR(pg_catalog.||) ' '::text OPERATOR(pg_catalog.||) "),
        ),
    }
}

impl PreparedFuzzySource {
    pub(crate) fn qualified_table(&self) -> String {
        quote_qualified_identifier(&self.schema_name, &self.table_name)
    }

    /// Returns the trigram text expression for this source.
    pub(crate) fn text_sql(&self, alias: Option<&str>) -> String {
        column_sql(&self.text_column_name, alias)
    }

    /// Returns the schema-qualified `pg_trgm` operator class for an access method.
    pub(crate) fn operator_class(&self, access_method: LexicalIndexAm) -> String {
        let class = match access_method {
            LexicalIndexAm::Gin => "gin_trgm_ops",
            LexicalIndexAm::Gist => "gist_trgm_ops",
        };
        quote_qualified_identifier(&self.trgm_schema_name, class)
    }

    /// Returns the schema-qualified `pg_trgm` similarity function for a mode.
    pub(crate) fn similarity_function(&self, function_name: &str) -> String {
        quote_qualified_identifier(&self.trgm_schema_name, function_name)
    }
}

fn column_sql(column_name: &str, alias: Option<&str>) -> String {
    match alias {
        Some(alias) => format!("{alias}.{}", quote_identifier(column_name)),
        None => quote_identifier(column_name),
    }
}

fn field_text_sql(field: &LexicalFieldBinding, alias: Option<&str>) -> String {
    let column = column_sql(&field.column_name, alias);
    match &field.json_path {
        None => format!("coalesce({column}::text, ''::text)"),
        Some(path) => {
            let literals = path
                .iter()
                .map(|component| quote_literal(component))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "coalesce(({column}) OPERATOR(pg_catalog.#>>) ARRAY[{literals}]::text[], ''::text)"
            )
        }
    }
}

/// Quotes a value as a PostgreSQL string literal through `format('%L', ...)`.
pub(crate) fn quote_literal(value: &str) -> String {
    Spi::get_one_with_args::<String>("SELECT pg_catalog.format('%L', $1)", &[value.into()])
        .unwrap_or_else(|error| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to quote lexical literal: {error}"),
            )
        })
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                "quoted lexical literal returned null",
            )
        })
}

fn port_failure(message: impl Into<String>) -> QueryError {
    QueryError::PortFailure {
        stage: "lexical_source_catalog",
        message: message.into(),
    }
}

fn spi_error(error: impl core::fmt::Display) -> QueryError {
    port_failure(error.to_string())
}

/// Loads and revalidates a registered lexical source for query execution.
///
/// Reads route through the membership-filtered view, so a non-member never
/// observes registration metadata. Every stored identity is rechecked against
/// the live catalog and any drift fails closed before Q1 execution starts.
pub(crate) fn prepare_lexical_source(
    collection_id: i64,
    source_name: &str,
) -> QueryResult<PreparedLexicalSource> {
    let mut source = load_lexical_source(collection_id, source_name)?;
    validate_source_relation(
        source.table_oid,
        &source.schema_name,
        &source.table_name,
        "lexical",
    )?;
    require_source_select(source.table_oid, &source.schema_name, &source.table_name)?;
    validate_lexical_document(&source)?;
    if let Some(index) = source.index.take() {
        match validate_lexical_index_binding(&source, &index)? {
            true => source.index = Some(index),
            false => source.index = None,
        }
    }
    Ok(source)
}

/// Loads and revalidates a registered fuzzy source for query execution.
pub(crate) fn prepare_fuzzy_source(
    collection_id: i64,
    source_name: &str,
) -> QueryResult<PreparedFuzzySource> {
    let mut source = load_fuzzy_source(collection_id, source_name)?;
    validate_source_relation(
        source.table_oid,
        &source.schema_name,
        &source.table_name,
        "fuzzy",
    )?;
    require_source_select(source.table_oid, &source.schema_name, &source.table_name)?;
    validate_fuzzy_column(&source)?;
    if let Some(index) = source.index.take() {
        match validate_fuzzy_index_binding(&source, &index)? {
            true => source.index = Some(index),
            false => source.index = None,
        }
    }
    Ok(source)
}

/// Proves that a newly attached lexical expression is planner-equivalent to
/// the registered document before the catalog write can commit.
pub(crate) fn validate_lexical_index_attachment(
    collection_id: i64,
    source_name: &str,
) -> QueryResult<()> {
    let source = prepare_lexical_source(collection_id, source_name)?;
    let index = source
        .index
        .as_ref()
        .ok_or_else(|| port_failure("lexical index attachment did not persist a usable index"))?;
    if matches!(source.document, LexicalDocument::Fields(_))
        && !planner_uses_lexical_index(&source, index)?
    {
        return Err(port_failure(format!(
            "lexical index does not match the registered document expression: {}",
            index.index_name
        )));
    }
    Ok(())
}

fn load_lexical_source(
    collection_id: i64,
    source_name: &str,
) -> QueryResult<PreparedLexicalSource> {
    let source = Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT lexical_source_id,
                        source_table_oid,
                        source_schema_name,
                        source_table_name,
                        document_mode,
                        stored_vector_column_name,
                        stored_vector_attnum,
                        configuration_schema_name,
                        configuration_name,
                        ranker,
                        normalization,
                        rank_weight_d,
                        rank_weight_c,
                        rank_weight_b,
                        rank_weight_a,
                        row_tsquery_name,
                        row_tsquery_column_name,
                        index_oid,
                        index_name,
                        index_am_name,
                        index_definition,
                        index_is_lossy,
                        registration_revision,
                        status
                   FROM pgcontext._visible_collection_lexical_sources
                  WHERE collection_id = $1 AND source_name = $2",
                Some(1),
                &[collection_id.into(), source_name.into()],
            )
            .map_err(spi_error)?;
        let Some(row) = rows.into_iter().next() else {
            return Err(port_failure(format!(
                "lexical source is not registered or not visible: {source_name}"
            )));
        };
        let status = required::<String>(&row, 24, "status")?;
        if status != "ready" {
            return Err(port_failure(format!(
                "lexical source {source_name} is {status}"
            )));
        }
        let document_mode = required::<String>(&row, 5, "document_mode")?;
        let document = match document_mode.as_str() {
            "stored_vector" => LexicalDocument::StoredVector {
                column_name: required::<String>(&row, 6, "stored_vector_column_name")?,
                attnum: required::<i16>(&row, 7, "stored_vector_attnum")?,
            },
            _ => LexicalDocument::Fields(Vec::new()),
        };
        let ranker = LexicalRanker::parse(&required::<String>(&row, 10, "ranker")?)?;
        let normalization = LexicalNormalization::new(
            u32::try_from(required::<i32>(&row, 11, "normalization")?)
                .map_err(|_| port_failure("registered normalization is negative"))?,
        )?;
        let rank_weights = LexicalRankWeights::new(
            required::<f32>(&row, 12, "rank_weight_d")?,
            required::<f32>(&row, 13, "rank_weight_c")?,
            required::<f32>(&row, 14, "rank_weight_b")?,
            required::<f32>(&row, 15, "rank_weight_a")?,
        )?;
        let row_tsquery = match (optional::<String>(&row, 16)?, optional::<String>(&row, 17)?) {
            (Some(name), Some(column)) => Some((name, column)),
            _ => None,
        };
        let index = match (
            optional::<pg_sys::Oid>(&row, 18)?,
            optional::<String>(&row, 19)?,
            optional::<String>(&row, 20)?,
            optional::<String>(&row, 21)?,
        ) {
            (Some(index_oid), Some(index_name), Some(am_name), Some(definition)) => {
                Some(LexicalIndexBinding {
                    index_oid,
                    index_name,
                    access_method: LexicalIndexAm::parse(&am_name).ok_or_else(|| {
                        port_failure("registered lexical index access method is unsupported")
                    })?,
                    definition,
                    is_lossy: required::<bool>(&row, 22, "index_is_lossy")?,
                })
            }
            _ => None,
        };
        Ok(PreparedLexicalSource {
            lexical_source_id: required::<i64>(&row, 1, "lexical_source_id")?,
            collection_id,
            source_name: source_name.to_owned(),
            table_oid: required::<pg_sys::Oid>(&row, 2, "source_table_oid")?,
            schema_name: required::<String>(&row, 3, "source_schema_name")?,
            table_name: required::<String>(&row, 4, "source_table_name")?,
            configuration_schema_name: required::<String>(&row, 8, "configuration_schema_name")?,
            configuration_name: required::<String>(&row, 9, "configuration_name")?,
            document,
            ranker,
            normalization,
            rank_weights,
            row_tsquery,
            index,
            registration_revision: required::<i64>(&row, 23, "registration_revision")?,
            rendered: RenderedLexicalSql::default(),
        })
    })?;
    load_lexical_fields(source)
}

fn load_lexical_fields(mut source: PreparedLexicalSource) -> QueryResult<PreparedLexicalSource> {
    if matches!(source.document, LexicalDocument::StoredVector { .. }) {
        source.render();
        return Ok(source);
    }
    let fields = Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT column_name,
                        column_attnum,
                        column_type_oid,
                        column_collation_oid,
                        json_path,
                        weight
                   FROM pgcontext._visible_collection_lexical_fields
                  WHERE lexical_source_id = $1
                  ORDER BY field_ordinal",
                None,
                &[source.lexical_source_id.into()],
            )
            .map_err(spi_error)?;
        let mut fields = Vec::new();
        for row in rows {
            // A NULL path element would silently change which JSON value the
            // document reads, so a partially-NULL or empty path fails closed.
            let json_path = match row.get::<Vec<Option<String>>>(5).map_err(spi_error)? {
                None => None,
                Some(path) => {
                    let components =
                        path.into_iter()
                            .collect::<Option<Vec<_>>>()
                            .ok_or_else(|| {
                                port_failure(
                                    "registered lexical JSON path contains a null component",
                                )
                            })?;
                    if components.is_empty() || components.len() > MAX_LEXICAL_JSON_PATH_DEPTH {
                        return Err(port_failure(
                            "registered lexical JSON path is empty or exceeds its depth bound",
                        ));
                    }
                    Some(components)
                }
            };
            fields.push(LexicalFieldBinding {
                column_name: required::<String>(&row, 1, "column_name")?,
                attnum: required::<i16>(&row, 2, "column_attnum")?,
                type_oid: required::<pg_sys::Oid>(&row, 3, "column_type_oid")?,
                collation_oid: required::<pg_sys::Oid>(&row, 4, "column_collation_oid")?,
                json_path,
                weight: LexicalWeight::parse(
                    &required::<String>(&row, 6, "weight")?.to_lowercase(),
                )?,
            });
        }
        Ok::<_, QueryError>(fields)
    })?;
    if fields.is_empty() || fields.len() > MAX_LEXICAL_FIELDS {
        return Err(port_failure(
            "registered lexical source has no usable field bindings",
        ));
    }
    source.document = LexicalDocument::Fields(fields);
    source.render();
    Ok(source)
}

fn load_fuzzy_source(collection_id: i64, source_name: &str) -> QueryResult<PreparedFuzzySource> {
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT fuzzy_source_id,
                        source_table_oid,
                        source_schema_name,
                        source_table_name,
                        text_column_name,
                        text_attnum,
                        text_type_oid,
                        text_collation_oid,
                        trgm_extension_oid,
                        trgm_schema_name,
                        index_oid,
                        index_name,
                        index_am_name,
                        index_definition,
                        registration_revision,
                        status
                   FROM pgcontext._visible_collection_fuzzy_sources
                  WHERE collection_id = $1 AND source_name = $2",
                Some(1),
                &[collection_id.into(), source_name.into()],
            )
            .map_err(spi_error)?;
        let Some(row) = rows.into_iter().next() else {
            return Err(port_failure(format!(
                "fuzzy source is not registered or not visible: {source_name}"
            )));
        };
        let status = required::<String>(&row, 16, "status")?;
        if status != "ready" {
            return Err(port_failure(format!(
                "fuzzy source {source_name} is {status}"
            )));
        }
        let extension_oid = required::<pg_sys::Oid>(&row, 9, "trgm_extension_oid")?;
        let trgm_schema_name = required::<String>(&row, 10, "trgm_schema_name")?;
        require_trgm_extension(extension_oid, &trgm_schema_name)?;
        let index = match (
            optional::<pg_sys::Oid>(&row, 11)?,
            optional::<String>(&row, 12)?,
            optional::<String>(&row, 13)?,
            optional::<String>(&row, 14)?,
        ) {
            (Some(index_oid), Some(index_name), Some(am_name), Some(definition)) => {
                Some(LexicalIndexBinding {
                    index_oid,
                    index_name,
                    access_method: LexicalIndexAm::parse(&am_name).ok_or_else(|| {
                        port_failure("registered fuzzy index access method is unsupported")
                    })?,
                    definition,
                    is_lossy: true,
                })
            }
            _ => None,
        };
        Ok(PreparedFuzzySource {
            collection_id,
            table_oid: required::<pg_sys::Oid>(&row, 2, "source_table_oid")?,
            schema_name: required::<String>(&row, 3, "source_schema_name")?,
            table_name: required::<String>(&row, 4, "source_table_name")?,
            text_column_name: required::<String>(&row, 5, "text_column_name")?,
            text_attnum: required::<i16>(&row, 6, "text_attnum")?,
            text_type_oid: required::<pg_sys::Oid>(&row, 7, "text_type_oid")?,
            text_collation_oid: required::<pg_sys::Oid>(&row, 8, "text_collation_oid")?,
            trgm_extension_oid: extension_oid,
            trgm_schema_name,
            index,
            registration_revision: required::<i64>(&row, 15, "registration_revision")?,
        })
    })
}

fn require_trgm_extension(extension_oid: pg_sys::Oid, schema_name: &str) -> QueryResult<()> {
    let matches = Spi::get_one_with_args::<bool>(
        "SELECT namespace.nspname = $2
           FROM pg_catalog.pg_extension AS extension
           JOIN pg_catalog.pg_namespace AS namespace
             ON namespace.oid = extension.extnamespace
          WHERE extension.oid = $1 AND extension.extname = 'pg_trgm'",
        &[extension_oid.into(), schema_name.into()],
    )
    .map_err(spi_error)?
    .unwrap_or(false);
    if matches {
        Ok(())
    } else {
        Err(port_failure(
            "registered pg_trgm extension is missing or was relocated",
        ))
    }
}

fn validate_source_relation(
    table_oid: pg_sys::Oid,
    schema_name: &str,
    table_name: &str,
    kind: &'static str,
) -> QueryResult<()> {
    let matches = Spi::get_one_with_args::<bool>(
        "SELECT namespace.nspname = $2 AND source_class.relname = $3
           FROM pg_catalog.pg_class AS source_class
           JOIN pg_catalog.pg_namespace AS namespace
             ON namespace.oid = source_class.relnamespace
          WHERE source_class.oid = $1 AND source_class.relkind IN ('r', 'p')",
        &[table_oid.into(), schema_name.into(), table_name.into()],
    )
    .map_err(spi_error)?
    .unwrap_or(false);
    if matches {
        Ok(())
    } else {
        Err(port_failure(format!(
            "registered {kind} source relation drifted: {schema_name}.{table_name}"
        )))
    }
}

fn require_source_select(
    table_oid: pg_sys::Oid,
    schema_name: &str,
    table_name: &str,
) -> QueryResult<()> {
    let has_select = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.has_table_privilege(SESSION_USER, $1, 'SELECT')",
        &[table_oid.into()],
    )
    .map_err(spi_error)?
    .unwrap_or(false);
    if has_select {
        Ok(())
    } else {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            format!("permission denied for source table: {schema_name}.{table_name}"),
        )
    }
}

fn validate_lexical_document(source: &PreparedLexicalSource) -> QueryResult<()> {
    validate_configuration(
        &source.configuration_schema_name,
        &source.configuration_name,
    )?;
    // A registered row `tsquery` binding is validated for every document mode,
    // so dropping that column fails closed with the drift SQLSTATE instead of a
    // raw PostgreSQL "column does not exist" from the rendered statement.
    if let Some((_, column_name)) = &source.row_tsquery {
        validate_typed_column(
            source.table_oid,
            column_name,
            None,
            "tsquery",
            "registered row tsquery column",
        )?;
    }
    match &source.document {
        LexicalDocument::StoredVector {
            column_name,
            attnum,
        } => validate_typed_column(
            source.table_oid,
            column_name,
            *attnum,
            "tsvector",
            "stored lexical document column",
        ),
        LexicalDocument::Fields(fields) => {
            for field in fields {
                validate_field_column(source.table_oid, field)?;
            }
            Ok(())
        }
    }
}

fn validate_configuration(schema_name: &str, configuration_name: &str) -> QueryResult<()> {
    let exists = Spi::get_one_with_args::<bool>(
        "SELECT true
           FROM pg_catalog.pg_ts_config AS configuration
           JOIN pg_catalog.pg_namespace AS namespace
             ON namespace.oid = configuration.cfgnamespace
          WHERE namespace.nspname = $1 AND configuration.cfgname = $2",
        &[schema_name.into(), configuration_name.into()],
    )
    .map_err(spi_error)?
    .unwrap_or(false);
    if exists {
        Ok(())
    } else {
        Err(port_failure(format!(
            "registered text search configuration drifted: {schema_name}.{configuration_name}"
        )))
    }
}

fn validate_field_column(table_oid: pg_sys::Oid, field: &LexicalFieldBinding) -> QueryResult<()> {
    let row = Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT attribute.attnum, attribute.atttypid, attribute.attcollation
                   FROM pg_catalog.pg_attribute AS attribute
                  WHERE attribute.attrelid = $1
                    AND attribute.attname = $2
                    AND attribute.attnum > 0
                    AND NOT attribute.attisdropped",
                Some(1),
                &[table_oid.into(), field.column_name.as_str().into()],
            )
            .map_err(spi_error)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        Ok::<_, QueryError>(Some((
            required::<i16>(&row, 1, "attnum")?,
            required::<pg_sys::Oid>(&row, 2, "atttypid")?,
            required::<pg_sys::Oid>(&row, 3, "attcollation")?,
        )))
    })?;
    let Some((attnum, type_oid, collation_oid)) = row else {
        return Err(port_failure(format!(
            "registered lexical field column is missing: {}",
            field.column_name
        )));
    };
    if attnum != field.attnum || type_oid != field.type_oid || collation_oid != field.collation_oid
    {
        return Err(port_failure(format!(
            "registered lexical field column drifted: {}",
            field.column_name
        )));
    }
    Ok(())
}

fn validate_fuzzy_column(source: &PreparedFuzzySource) -> QueryResult<()> {
    let identity = Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT attribute.attnum, attribute.atttypid, attribute.attcollation
                   FROM pg_catalog.pg_attribute AS attribute
                  WHERE attribute.attrelid = $1
                    AND attribute.attname = $2
                    AND attribute.attnum > 0
                    AND NOT attribute.attisdropped",
                Some(1),
                &[
                    source.table_oid.into(),
                    source.text_column_name.as_str().into(),
                ],
            )
            .map_err(spi_error)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        Ok::<_, QueryError>(Some((
            required::<i16>(&row, 1, "attnum")?,
            required::<pg_sys::Oid>(&row, 2, "atttypid")?,
            required::<pg_sys::Oid>(&row, 3, "attcollation")?,
        )))
    })?;
    if identity
        == Some((
            source.text_attnum,
            source.text_type_oid,
            source.text_collation_oid,
        ))
    {
        Ok(())
    } else {
        Err(port_failure(format!(
            "registered fuzzy source text column drifted: {}",
            source.text_column_name
        )))
    }
}

fn validate_typed_column(
    table_oid: pg_sys::Oid,
    column_name: &str,
    expected_attnum: impl Into<Option<i16>>,
    type_name: &'static str,
    label: &'static str,
) -> QueryResult<()> {
    let expected_attnum = expected_attnum.into().unwrap_or_default();
    let matches = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.format_type(attribute.atttypid, NULL) = $3
                AND ($4 = 0::int2 OR attribute.attnum = $4)
           FROM pg_catalog.pg_attribute AS attribute
          WHERE attribute.attrelid = $1
            AND attribute.attname = $2
            AND attribute.attnum > 0
            AND NOT attribute.attisdropped",
        &[
            table_oid.into(),
            column_name.into(),
            type_name.into(),
            expected_attnum.into(),
        ],
    )
    .map_err(spi_error)?
    .unwrap_or(false);
    if matches {
        Ok(())
    } else {
        Err(port_failure(format!(
            "{label} is missing or is not {type_name}: {column_name}"
        )))
    }
}

#[derive(Debug)]
struct ObservedIndex {
    indrelid: pg_sys::Oid,
    access_method: String,
    is_valid: bool,
    is_live: bool,
    is_full: bool,
    key_count: i16,
    attribute_count: i16,
    key_attnum: i16,
    has_expression: bool,
    opclass_oid: pg_sys::Oid,
    opclass_schema: String,
    opclass_name: String,
    definition: String,
}

/// Revalidates the lifecycle and structural identity shared by lexical and
/// fuzzy indexes. A dropped index detaches to the exact fallback; semantic
/// drift fails closed.
fn observe_index(index: &LexicalIndexBinding) -> QueryResult<Option<ObservedIndex>> {
    let observed = Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT index.indrelid,
                        access_method.amname::text,
                        index.indisvalid,
                        index.indislive,
                        index.indpred IS NULL,
                        index.indnkeyatts,
                        index.indnatts,
                        index.indkey[0],
                        index.indexprs IS NOT NULL,
                        operator_class.oid,
                        operator_namespace.nspname::text,
                        operator_class.opcname::text,
                        pg_catalog.pg_get_indexdef(index.indexrelid)
                   FROM pg_catalog.pg_index AS index
                   JOIN pg_catalog.pg_class AS index_class
                     ON index_class.oid = index.indexrelid
                   JOIN pg_catalog.pg_am AS access_method
                     ON access_method.oid = index_class.relam
                   JOIN pg_catalog.pg_opclass AS operator_class
                     ON operator_class.oid = index.indclass[0]
                   JOIN pg_catalog.pg_namespace AS operator_namespace
                     ON operator_namespace.oid = operator_class.opcnamespace
                  WHERE index.indexrelid = $1",
                Some(1),
                &[index.index_oid.into()],
            )
            .map_err(spi_error)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        Ok::<_, QueryError>(Some(ObservedIndex {
            indrelid: required::<pg_sys::Oid>(&row, 1, "indrelid")?,
            access_method: required::<String>(&row, 2, "amname")?,
            is_valid: required::<bool>(&row, 3, "indisvalid")?,
            is_live: required::<bool>(&row, 4, "indislive")?,
            is_full: required::<bool>(&row, 5, "is_full")?,
            key_count: required::<i16>(&row, 6, "indnkeyatts")?,
            attribute_count: required::<i16>(&row, 7, "indnatts")?,
            key_attnum: required::<i16>(&row, 8, "indkey")?,
            has_expression: required::<bool>(&row, 9, "has_expression")?,
            opclass_oid: required::<pg_sys::Oid>(&row, 10, "opclass_oid")?,
            opclass_schema: required::<String>(&row, 11, "opclass_schema")?,
            opclass_name: required::<String>(&row, 12, "opclass_name")?,
            definition: required::<String>(&row, 13, "indexdef")?,
        }))
    })?;
    Ok(observed)
}

fn validate_index_lifecycle(
    table_oid: pg_sys::Oid,
    index: &LexicalIndexBinding,
) -> QueryResult<Option<ObservedIndex>> {
    let Some(observed) = observe_index(index)? else {
        return Ok(None);
    };
    if observed.indrelid != table_oid || observed.access_method != index.access_method.stable_name()
    {
        return Err(port_failure(format!(
            "attached lexical index drifted from its registered relation or access method: {}",
            index.index_name
        )));
    }
    if observed.definition != index.definition {
        return Err(port_failure(format!(
            "attached lexical index definition drifted: {}",
            index.index_name
        )));
    }
    if !observed.is_full {
        return Err(port_failure(format!(
            "attached lexical index is partial: {}",
            index.index_name
        )));
    }
    if observed.key_count != 1 || observed.attribute_count != 1 {
        return Err(port_failure(format!(
            "attached lexical index must contain exactly one key and no included columns: {}",
            index.index_name
        )));
    }
    if !observed.is_valid || !observed.is_live {
        return Ok(None);
    }
    Ok(Some(observed))
}

fn validate_lexical_index_binding(
    source: &PreparedLexicalSource,
    index: &LexicalIndexBinding,
) -> QueryResult<bool> {
    let Some(observed) = validate_index_lifecycle(source.table_oid, index)? else {
        return Ok(false);
    };
    let expected_key_shape = match &source.document {
        LexicalDocument::StoredVector { attnum, .. } => {
            observed.key_attnum == *attnum && !observed.has_expression
        }
        LexicalDocument::Fields(fields) => {
            let expected = fields
                .iter()
                .map(|field| field.attnum)
                .collect::<BTreeSet<_>>();
            observed.key_attnum == 0
                && observed.has_expression
                && observed_index_columns(index.index_oid, source.table_oid)? == expected
        }
    };
    if !expected_key_shape
        || observed.opclass_schema != "pg_catalog"
        || observed.opclass_name != "tsvector_ops"
    {
        return Err(port_failure(format!(
            "lexical index does not match the registered document expression: {}",
            index.index_name
        )));
    }
    Ok(true)
}

fn validate_fuzzy_index_binding(
    source: &PreparedFuzzySource,
    index: &LexicalIndexBinding,
) -> QueryResult<bool> {
    let Some(observed) = validate_index_lifecycle(source.table_oid, index)? else {
        return Ok(false);
    };
    let expected_opclass = match index.access_method {
        LexicalIndexAm::Gin => "gin_trgm_ops",
        LexicalIndexAm::Gist => "gist_trgm_ops",
    };
    let extension_owned = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS (
             SELECT 1
               FROM pg_catalog.pg_depend AS dependency
              WHERE dependency.classid = 'pg_catalog.pg_opclass'::pg_catalog.regclass
                AND dependency.objid = $1
                AND dependency.refclassid = 'pg_catalog.pg_extension'::pg_catalog.regclass
                AND dependency.refobjid = $2
                AND dependency.deptype = 'e'
         )",
        &[
            observed.opclass_oid.into(),
            source.trgm_extension_oid.into(),
        ],
    )
    .map_err(spi_error)?
    .unwrap_or(false);
    if observed.key_attnum != source.text_attnum
        || observed.has_expression
        || observed.opclass_schema != source.trgm_schema_name
        || observed.opclass_name != expected_opclass
        || !extension_owned
    {
        return Err(port_failure(format!(
            "fuzzy index does not match the registered text column and pg_trgm operator class: {}",
            index.index_name
        )));
    }
    Ok(true)
}

fn observed_index_columns(
    index_oid: pg_sys::Oid,
    table_oid: pg_sys::Oid,
) -> QueryResult<BTreeSet<i16>> {
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT DISTINCT dependency.refobjsubid::int2
                   FROM pg_catalog.pg_depend AS dependency
                  WHERE dependency.classid = 'pg_catalog.pg_class'::pg_catalog.regclass
                    AND dependency.objid = $1
                    AND dependency.refclassid = 'pg_catalog.pg_class'::pg_catalog.regclass
                    AND dependency.refobjid = $2
                    AND dependency.refobjsubid > 0",
                None,
                &[index_oid.into(), table_oid.into()],
            )
            .map_err(spi_error)?;
        rows.into_iter()
            .map(|row| required::<i16>(&row, 1, "index_column_attnum"))
            .collect::<QueryResult<BTreeSet<_>>>()
    })
}

fn planner_uses_lexical_index(
    source: &PreparedLexicalSource,
    index: &LexicalIndexBinding,
) -> QueryResult<bool> {
    let previous = Spi::get_one::<String>("SELECT pg_catalog.current_setting('enable_seqscan')")
        .map_err(spi_error)?
        .ok_or_else(|| port_failure("enable_seqscan setting returned null"))?;
    set_local_setting("enable_seqscan", "off")?;
    let outcome = explain_uses_lexical_index(source, index);
    let restored = set_local_setting("enable_seqscan", &previous);
    match (outcome, restored) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(uses_index), Ok(())) => Ok(uses_index),
    }
}

fn explain_uses_lexical_index(
    source: &PreparedLexicalSource,
    index: &LexicalIndexBinding,
) -> QueryResult<bool> {
    let query_text = quote_literal("pgcontext index validation");
    let sql = format!(
        "EXPLAIN (FORMAT JSON, COSTS OFF)
         SELECT 1
           FROM {table} AS {alias}
          WHERE {document} OPERATOR(pg_catalog.@@)
                pg_catalog.plainto_tsquery({configuration}, {query_text})",
        table = source.qualified_table(),
        alias = SOURCE_ALIAS,
        document = source.document_sql(),
        configuration = source.configuration_sql(),
    );
    Spi::connect(|client| {
        let rows = client.select(&sql, Some(1), &[]).map_err(spi_error)?;
        let Some(row) = rows.into_iter().next() else {
            return Err(port_failure(
                "lexical index validation plan returned no row",
            ));
        };
        let plan = required::<pgrx::Json>(&row, 1, "query_plan")?;
        Ok(json_uses_index(&plan.0, &index.index_name))
    })
}

fn set_local_setting(setting: &str, value: &str) -> QueryResult<()> {
    Spi::get_one_with_args::<String>(
        "SELECT pg_catalog.set_config($1, $2, true)",
        &[setting.into(), value.into()],
    )
    .map(|_| ())
    .map_err(spi_error)
}

fn json_uses_index(value: &serde_json::Value, index_name: &str) -> bool {
    match value {
        serde_json::Value::Object(object) => {
            object.get("Index Name").and_then(serde_json::Value::as_str) == Some(index_name)
                || object
                    .values()
                    .any(|child| json_uses_index(child, index_name))
        }
        serde_json::Value::Array(values) => values
            .iter()
            .any(|child| json_uses_index(child, index_name)),
        _ => false,
    }
}

fn required<T>(row: &spi::SpiHeapTupleData<'_>, index: usize, label: &'static str) -> QueryResult<T>
where
    T: FromDatum + IntoDatum,
{
    optional::<T>(row, index)?
        .ok_or_else(|| port_failure(format!("catalog column is null: {label}")))
}

fn optional<T>(row: &spi::SpiHeapTupleData<'_>, index: usize) -> QueryResult<Option<T>>
where
    T: FromDatum + IntoDatum,
{
    row.get::<T>(index).map_err(spi_error)
}

/// Resolves the collection identifier and enforces collection ownership.
pub(crate) fn require_collection_owner_id(collection: &str) -> i64 {
    let collection_name = CollectionName::new(collection.to_owned())
        .unwrap_or_else(|error| raise_core_invalid(&error.to_string()));
    let resolved = resolve_collection(&collection_name);
    crate::table_search::require_collection_owner(&resolved, &collection_name);
    resolved.collection_id
}

fn raise_core_invalid(message: &str) -> ! {
    raise_sql_error(
        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
        message.to_owned(),
    )
}
