use super::*;

#[derive(Clone, Debug)]
pub(super) struct ResolvedSource {
    pub(super) oid: pg_sys::Oid,
    pub(super) schema_name: String,
    pub(super) table_name: String,
}

#[derive(Clone, Debug)]
pub(super) struct InspectedColumn {
    pub(super) attnum: i16,
    pub(super) name: String,
    pub(super) type_oid: pg_sys::Oid,
    pub(super) type_schema: String,
    pub(super) type_name: String,
    pub(super) formatted_type: String,
    pub(super) typmod: i32,
    pub(super) collation_oid: pg_sys::Oid,
    pub(super) not_null: bool,
    pub(super) type_kind: String,
    pub(super) element_type_oid: pg_sys::Oid,
    pub(super) postgis_member: bool,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct ColumnClassification {
    pub(super) family: Option<&'static str>,
    pub(super) reason: &'static str,
}

#[derive(Clone, Debug)]
pub(super) struct SourceKeyBinding {
    pub(super) column: InspectedColumn,
    pub(super) unique_index_oid: pg_sys::Oid,
}

#[derive(Clone, Debug)]
pub(super) struct ColumnBinding {
    pub(super) ordinal: i16,
    pub(super) name: String,
    pub(super) kind: String,
    pub(super) column: InspectedColumn,
    pub(super) dimensions: Option<i32>,
    pub(super) metric: Option<String>,
    pub(super) text_configuration_oid: Option<pg_sys::Oid>,
    pub(super) normalization: Option<String>,
}

pub(super) fn resolve_source(source_table: &str) -> ResolvedSource {
    let qualified = QualifiedTableName::new(source_table).unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            error.to_string(),
        )
    });
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT class.oid, namespace.nspname::text, class.relname::text
                   FROM pg_catalog.pg_class AS class
                   JOIN pg_catalog.pg_namespace AS namespace
                     ON namespace.oid = class.relnamespace
                  WHERE class.oid = pg_catalog.to_regclass($1)
                    AND class.relkind IN ('r','p')",
                Some(1),
                &[qualified.as_qualified_name().as_str().into()],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_UNDEFINED_TABLE,
                    format!("failed to resolve exact-first source table: {error}"),
                )
            });
        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_TABLE,
                "exact-first source table does not exist or is not an ordinary/partitioned table",
            );
        }
        let row = rows.first();
        ResolvedSource {
            oid: required(
                row.get::<pg_sys::Oid>(1).unwrap_or(None),
                "source_table_oid",
            ),
            schema_name: required(row.get::<String>(2).unwrap_or(None), "source_schema_name"),
            table_name: required(row.get::<String>(3).unwrap_or(None), "source_table_name"),
        }
    })
}

pub(super) fn inspect_columns(source_oid: pg_sys::Oid) -> Vec<InspectedColumn> {
    require_index_bound(source_oid);
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT attribute.attnum,
                        attribute.attname::text,
                        attribute.atttypid,
                        namespace.nspname::text,
                        type.typname::text,
                        pg_catalog.format_type(attribute.atttypid, attribute.atttypmod),
                        attribute.atttypmod,
                        attribute.attcollation,
                        attribute.attnotnull,
                        type.typtype::text,
                        type.typelem,
                        EXISTS (
                            SELECT 1
                              FROM pg_catalog.pg_depend AS dependency
                              JOIN pg_catalog.pg_extension AS extension
                                ON extension.oid = dependency.refobjid
                             WHERE dependency.classid = 'pg_catalog.pg_type'::regclass
                               AND dependency.objid = attribute.atttypid
                               AND dependency.refclassid = 'pg_catalog.pg_extension'::regclass
                               AND extension.extname = 'postgis'
                        )
                   FROM pg_catalog.pg_attribute AS attribute
                   JOIN pg_catalog.pg_type AS type ON type.oid = attribute.atttypid
                   JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = type.typnamespace
                  WHERE attribute.attrelid = $1
                    AND attribute.attnum > 0
                    AND NOT attribute.attisdropped
                  ORDER BY attribute.attnum
                  LIMIT $2",
                None,
                &[
                    source_oid.into(),
                    i64::try_from(MAX_COLUMNS + 1).unwrap_or(i64::MAX).into(),
                ],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to inspect exact-first source columns: {error}"),
                )
            });
        if rows.len() > MAX_COLUMNS {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "exact-first source exceeds the column inspection bound",
            );
        }
        rows.into_iter()
            .map(|row| InspectedColumn {
                attnum: required(row.get::<i16>(1).unwrap_or(None), "column_attnum"),
                name: required(row.get::<String>(2).unwrap_or(None), "column_name"),
                type_oid: required(row.get::<pg_sys::Oid>(3).unwrap_or(None), "column_type_oid"),
                type_schema: required(row.get::<String>(4).unwrap_or(None), "type_schema"),
                type_name: required(row.get::<String>(5).unwrap_or(None), "type_name"),
                formatted_type: required(row.get::<String>(6).unwrap_or(None), "formatted_type"),
                typmod: required(row.get::<i32>(7).unwrap_or(None), "column_typmod"),
                collation_oid: required(
                    row.get::<pg_sys::Oid>(8).unwrap_or(None),
                    "column_collation_oid",
                ),
                not_null: required(row.get::<bool>(9).unwrap_or(None), "column_not_null"),
                type_kind: required(row.get::<String>(10).unwrap_or(None), "column_type_kind"),
                element_type_oid: required(
                    row.get::<pg_sys::Oid>(11).unwrap_or(None),
                    "column_element_type_oid",
                ),
                postgis_member: required(
                    row.get::<bool>(12).unwrap_or(None),
                    "column_postgis_member",
                ),
            })
            .collect()
    })
}

fn require_index_bound(source_oid: pg_sys::Oid) {
    let observed = Spi::get_one_with_args::<i64>(
        "SELECT count(*)
           FROM (
                SELECT 1
                  FROM pg_catalog.pg_index
                 WHERE indrelid = $1
                 LIMIT $2
           ) AS bounded_indexes",
        &[
            source_oid.into(),
            i64::try_from(MAX_INDEXES + 1).unwrap_or(i64::MAX).into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to bound exact-first source indexes: {error}"),
        )
    })
    .unwrap_or(0);
    if observed > i64::try_from(MAX_INDEXES).unwrap_or(i64::MAX) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "exact-first source exceeds the index inspection bound",
        );
    }
}

pub(super) fn classify_column(column: &InspectedColumn) -> ColumnClassification {
    if column.type_kind != "b" {
        return unsupported("domains, composites, and pseudo-types require an explicit adapter");
    }
    if column.type_schema == "pgcontext" {
        return match column.type_name.as_str() {
            "vector" => supported("vector"),
            "halfvec" => supported("halfvec"),
            "sparsevec" => supported("sparsevec"),
            "int8vec" => supported("int8vec"),
            "uint8vec" => supported("uint8vec"),
            "bitvec" => supported("bitvec"),
            "_vector" if column.element_type_oid != pg_sys::Oid::INVALID => {
                supported("vector_array")
            }
            _ => unsupported("pgContext type is not a certified exact-first source"),
        };
    }
    if column.type_schema == "pg_catalog" {
        return match column.type_name.as_str() {
            "text" | "varchar" | "bpchar" => supported("text"),
            "tsvector" => supported("tsvector"),
            "bool" | "int2" | "int4" | "int8" | "float4" | "float8" | "numeric" => {
                supported("scalar")
            }
            "jsonb" => supported("jsonb"),
            "date" | "timestamp" | "timestamptz" | "uuid" => supported("temporal_uuid"),
            _ => unsupported("column type has no certified exact-first adapter"),
        };
    }
    if column.postgis_member && matches!(column.type_name.as_str(), "geometry" | "geography") {
        return supported("optional_postgis");
    }
    unsupported("column type has no certified exact-first adapter")
}

pub(super) fn resolve_source_key(
    source: &ResolvedSource,
    columns: &[InspectedColumn],
    key_column: &str,
) -> SourceKeyBinding {
    let column = columns
        .iter()
        .find(|column| column.name == key_column)
        .cloned()
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
                "exact-first source-key column does not exist",
            )
        });
    if !column.not_null || !canonical_key_type(&column) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "exact-first source key must be non-null bigint/integer/smallint/text/varchar/uuid",
        );
    }
    let index_oid = Spi::get_one_with_args::<pg_sys::Oid>(
        "SELECT index.indexrelid
           FROM pg_catalog.pg_index AS index
          WHERE index.indrelid = $1
            AND index.indisunique AND index.indisvalid AND index.indisready
            AND index.indislive AND index.indimmediate
            AND index.indpred IS NULL AND index.indexprs IS NULL
            AND index.indnkeyatts = 1 AND index.indnatts = 1
            AND $2 = ANY(index.indkey)
          ORDER BY index.indisprimary DESC, index.indexrelid
          LIMIT 1",
        &[source.oid.into(), column.attnum.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to inspect exact-first source-key index: {error}"),
        )
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "exact-first source key requires a live immediate nonpartial single-column unique index",
        )
    });
    SourceKeyBinding {
        column,
        unique_index_oid: index_oid,
    }
}

pub(super) fn resolve_binding(
    ordinal: usize,
    column: &InspectedColumn,
    specification: &ExactFirstBindingSpecification,
) -> ColumnBinding {
    let expected = expected_family(&specification.kind).unwrap_or_else(|| {
        invalid_specification(format!(
            "unsupported exact-first binding kind: {}",
            specification.kind
        ))
    });
    let actual = classify_column(column).family.unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATATYPE_MISMATCH,
            format!("exact-first binding column is unsupported: {}", column.name),
        )
    });
    if !family_matches(expected, actual) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATATYPE_MISMATCH,
            format!(
                "exact-first binding kind {} does not match column {} ({actual})",
                specification.kind, column.name
            ),
        );
    }
    validate_metric_and_dimensions(column, specification);
    let text_configuration_oid = specification
        .text_configuration
        .as_ref()
        .map(|configuration| resolve_text_configuration(configuration));
    ColumnBinding {
        ordinal: i16::try_from(ordinal).unwrap_or_else(|_| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "exact-first binding ordinal exceeds the catalog bound",
            )
        }),
        name: specification.name.clone(),
        kind: specification.kind.clone(),
        column: column.clone(),
        dimensions: specification.dimensions,
        metric: specification.metric.clone(),
        text_configuration_oid,
        normalization: specification.normalization.clone(),
    }
}

fn canonical_key_type(column: &InspectedColumn) -> bool {
    column.type_schema == "pg_catalog"
        && matches!(
            column.type_name.as_str(),
            "int2" | "int4" | "int8" | "text" | "varchar" | "uuid"
        )
}

fn expected_family(kind: &str) -> Option<&'static str> {
    match kind {
        "dense" => Some("vector"),
        "half" => Some("halfvec"),
        "sparse" => Some("sparsevec"),
        "int8" => Some("int8vec"),
        "uint8" => Some("uint8vec"),
        "bit" => Some("bitvec"),
        "vector_array" => Some("vector_array"),
        "lexical" | "fuzzy" => Some("text"),
        "tsvector" => Some("tsvector"),
        "filter" | "payload" => Some("filter_payload"),
        _ => None,
    }
}

fn family_matches(expected: &str, actual: &str) -> bool {
    expected == actual
        || (expected == "filter_payload"
            && matches!(
                actual,
                "scalar" | "text" | "jsonb" | "temporal_uuid" | "optional_postgis"
            ))
}

fn validate_metric_and_dimensions(
    column: &InspectedColumn,
    specification: &ExactFirstBindingSpecification,
) {
    let vector_kind = matches!(
        specification.kind.as_str(),
        "dense" | "half" | "sparse" | "int8" | "uint8" | "bit" | "vector_array"
    );
    if vector_kind {
        if specification
            .dimensions
            .is_none_or(|dimensions| dimensions <= 0)
        {
            invalid_specification("vector exact-first bindings require positive dimensions");
        }
        let metric = specification.metric.as_deref().unwrap_or_else(|| {
            invalid_specification("vector exact-first bindings require a metric")
        });
        let valid_metric = if specification.kind == "bit" {
            matches!(metric, "hamming" | "jaccard")
        } else {
            matches!(metric, "l2" | "inner_product" | "cosine" | "l1")
        };
        if !valid_metric {
            invalid_specification(
                "exact-first binding metric is incompatible with its representation",
            );
        }
        if column.typmod > 0
            && specification.kind != "vector_array"
            && specification.dimensions != Some(column.typmod)
        {
            invalid_specification("exact-first dimensions do not match the source typmod");
        }
    } else if specification.dimensions.is_some() || specification.metric.is_some() {
        invalid_specification(
            "non-vector exact-first bindings cannot declare dimensions or metric",
        );
    }
    if matches!(specification.kind.as_str(), "lexical" | "tsvector")
        != specification.text_configuration.is_some()
    {
        invalid_specification(
            "lexical exact-first bindings require a text configuration and other bindings forbid it",
        );
    }
}

fn resolve_text_configuration(value: &str) -> pg_sys::Oid {
    Spi::get_one_with_args::<pg_sys::Oid>("SELECT $1::pg_catalog.regconfig::oid", &[value.into()])
        .unwrap_or_else(|error| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!("failed to resolve exact-first text configuration: {error}"),
            )
        })
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                "exact-first text configuration does not exist",
            )
        })
}

const fn supported(family: &'static str) -> ColumnClassification {
    ColumnClassification {
        family: Some(family),
        reason: "certified exact-first source family",
    }
}

const fn unsupported(reason: &'static str) -> ColumnClassification {
    ColumnClassification {
        family: None,
        reason,
    }
}
