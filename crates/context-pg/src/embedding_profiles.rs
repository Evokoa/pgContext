//! Immutable provider embedding-profile catalog.

use std::collections::BTreeSet;

use context_core::{
    EmbeddingProfile, IntegerScale, MatryoshkaPolicy, PrefixDimensions, ProfileId,
    ProfileLifecycle, ProfileName, ProviderBinaryLayout, ProviderBitOrder, ProviderByteOrder,
    VectorNormalization, VectorRepresentation,
};
use pgrx::JsonB;
use pgrx::prelude::*;
use serde_json::{Map, Value, json};

use crate::domain_types::{distance_metric_label, parse_distance_metric};
use crate::error::raise_sql_error;
use crate::table_search::{quote_identifier, quote_qualified_identifier};

const PROFILE_KEYS: [&str; 17] = [
    "representation",
    "dimensions",
    "normalization",
    "metric",
    "provider",
    "model",
    "revision",
    "input_template",
    "output_template",
    "bit_order",
    "byte_order",
    "scale",
    "zero_point",
    "configuration_hash",
    "matryoshka_prefixes",
    "source_version_column",
    "embedding_version_column",
];

#[derive(Clone, Copy)]
struct ProfileCollection {
    collection_id: i64,
    owner_role: pg_sys::Oid,
    source_table_oid: pg_sys::Oid,
}

struct ProfileBinding {
    source_schema_name: String,
    source_table_name: String,
    source_column_name: String,
    source_attnum: i16,
    source_type_name: String,
    source_typmod: i32,
    version_columns: Option<VersionColumnBinding>,
    hnsw_schema_name: String,
    hnsw_index_name: String,
    hnsw_opclass: String,
}

struct VersionColumnNames {
    source: String,
    embedding: String,
}

struct VersionColumnBinding {
    source_name: String,
    source_attnum: i16,
    embedding_name: String,
    embedding_attnum: i16,
}

pub(crate) struct ProviderProfileContract {
    pub(crate) dimensions: usize,
    pub(crate) representation: VectorRepresentation,
    pub(crate) binary_layout: Option<ProviderBinaryLayout>,
}

/// Registers one immutable provider embedding profile.
#[pg_extern(security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn register_embedding_profile(
    collection: String,
    profile_name: String,
    source_column: String,
    hnsw_index: String,
    profile: JsonB,
    lifecycle: default!(String, "'active'"),
) -> JsonB {
    let profile_name =
        ProfileName::new(profile_name).unwrap_or_else(|error| invalid_profile(error.to_string()));
    let lifecycle = registration_lifecycle(&lifecycle);
    let collection_row = resolve_collection(&collection);
    require_collection_owner(collection_row, &collection);
    let normalized = parse_profile(&profile);
    let binding = resolve_profile_binding(
        collection_row,
        &collection,
        &source_column,
        &hnsw_index,
        &normalized,
    );
    reject_duplicate_profile(collection_row.collection_id, profile_name.as_str());
    insert_profile(
        collection_row.collection_id,
        profile_name.as_str(),
        &binding,
        &normalized,
        lifecycle,
    );
    JsonB(normalized.json())
}

/// Lists immutable embedding profiles visible through collection ownership.
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn embedding_profiles() -> TableIterator<
    'static,
    (
        name!(collection_name, String),
        name!(profile_name, String),
        name!(source_column, String),
        name!(hnsw_index, String),
        name!(profile, JsonB),
    ),
> {
    let rows = Spi::connect(|client| {
        let rows = client.select(
            "SELECT collections.collection_name,
                    profiles.profile_name,
                    profiles.source_column_name,
                    pg_catalog.format('%I.%I', profiles.hnsw_schema_name, profiles.hnsw_index_name),
                    pg_catalog.jsonb_build_object(
                        'representation', profiles.representation,
                        'dimensions', profiles.dimensions,
                        'normalization', profiles.normalization,
                        'metric', profiles.metric,
                        'provider', profiles.provider,
                        'model', profiles.model,
                        'revision', profiles.revision,
                        'input_template', profiles.input_template,
                        'output_template', profiles.output_template,
                        'bit_order', profiles.bit_order,
                        'byte_order', profiles.byte_order,
                        'scale', profiles.scale,
                        'zero_point', profiles.zero_point,
                        'configuration_hash', profiles.configuration_hash,
                        'matryoshka_prefixes', profiles.matryoshka_prefixes,
                        'source_version_column', profiles.source_version_column_name,
                        'embedding_version_column', profiles.embedding_version_column_name
                    )
               FROM pgcontext._embedding_profiles AS profiles
               JOIN pgcontext._collections AS collections USING (collection_id)
              WHERE pg_catalog.pg_has_role(SESSION_USER, collections.owner_role, 'MEMBER')
              ORDER BY collections.collection_name, profiles.profile_name",
            None,
            &[],
        )?;
        rows.into_iter()
            .map(|row| {
                Ok((
                    required(row.get::<String>(1)?, "collection_name"),
                    required(row.get::<String>(2)?, "profile_name"),
                    required(row.get::<String>(3)?, "source_column"),
                    required(row.get::<String>(4)?, "hnsw_index"),
                    required(row.get::<JsonB>(5)?, "profile"),
                ))
            })
            .collect::<Result<Vec<_>, spi::Error>>()
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to list embedding profiles: {error}"),
        )
    });
    TableIterator::new(rows)
}

/// Explains source authority and the compatible exact HNSW contract for a profile.
#[pg_extern(security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn embedding_profile_explain(collection: String, profile_name: String) -> JsonB {
    Spi::get_one_with_args::<JsonB>(
        "SELECT pg_catalog.jsonb_build_object(
                    'profile', pg_catalog.jsonb_build_object(
                        'representation', profiles.representation,
                        'dimensions', profiles.dimensions,
                        'normalization', profiles.normalization,
                        'metric', profiles.metric,
                        'provider', profiles.provider,
                        'model', profiles.model,
                        'revision', profiles.revision,
                        'input_template', profiles.input_template,
                        'output_template', profiles.output_template,
                        'bit_order', profiles.bit_order,
                        'byte_order', profiles.byte_order,
                        'scale', profiles.scale,
                        'zero_point', profiles.zero_point,
                        'configuration_hash', profiles.configuration_hash,
                        'matryoshka_prefixes', profiles.matryoshka_prefixes,
                        'source_version_column', profiles.source_version_column_name,
                        'embedding_version_column', profiles.embedding_version_column_name
                    ),
                    'source_column', pg_catalog.format(
                        '%I.%I.%I',
                        profiles.source_schema_name,
                        profiles.source_table_name,
                        profiles.source_column_name
                    ),
                    'hnsw_index', pg_catalog.format(
                        '%I.%I', profiles.hnsw_schema_name, profiles.hnsw_index_name
                    ),
                    'binding_valid', EXISTS (
                        SELECT 1
                          FROM pg_catalog.pg_class AS source_table
                          JOIN pg_catalog.pg_namespace AS source_namespace
                            ON source_namespace.oid = source_table.relnamespace
                          JOIN pg_catalog.pg_attribute AS source_attribute
                            ON source_attribute.attrelid = source_table.oid
                           AND source_attribute.attnum = profiles.source_attnum
                           AND source_attribute.attname = profiles.source_column_name
                           AND source_attribute.atttypmod = profiles.source_typmod
                          JOIN pg_catalog.pg_type AS source_type
                            ON source_type.oid = source_attribute.atttypid
                           AND source_type.typname = profiles.source_type_name
                          JOIN pg_catalog.pg_namespace AS source_type_namespace
                            ON source_type_namespace.oid = source_type.typnamespace
                           AND source_type_namespace.nspname = 'pgcontext'
                          JOIN pg_catalog.pg_class AS index_class
                            ON index_class.relname = profiles.hnsw_index_name
                          JOIN pg_catalog.pg_namespace AS index_namespace
                            ON index_namespace.oid = index_class.relnamespace
                           AND index_namespace.nspname = profiles.hnsw_schema_name
                          JOIN pg_catalog.pg_index AS index_catalog
                            ON index_catalog.indexrelid = index_class.oid
                           AND index_catalog.indrelid = source_table.oid
                           AND index_catalog.indkey[0] = profiles.source_attnum
                           AND index_catalog.indnkeyatts = 1
                           AND index_catalog.indnatts = 1
                           AND index_catalog.indexprs IS NULL
                           AND index_catalog.indpred IS NULL
                           AND index_catalog.indisvalid
                           AND index_catalog.indisready
                           AND index_catalog.indislive
                          JOIN pg_catalog.pg_am AS access_method
                            ON access_method.oid = index_class.relam
                           AND access_method.amname = 'pgcontext_hnsw'
                          JOIN pg_catalog.pg_opclass AS opclass
                            ON opclass.oid = index_catalog.indclass[0]
                           AND opclass.opcname = profiles.hnsw_opclass
                          JOIN pg_catalog.pg_namespace AS opclass_namespace
                            ON opclass_namespace.oid = opclass.opcnamespace
                           AND opclass_namespace.nspname = 'pgcontext'
                         WHERE source_namespace.nspname = profiles.source_schema_name
                           AND source_table.relname = profiles.source_table_name
                           AND source_table.oid = collections.source_table_oid
                    ),
                    'source_authority', CASE profiles.representation
                        WHEN 'int8' THEN 'provider_native'
                        WHEN 'uint8' THEN 'provider_native'
                        WHEN 'bit' THEN 'provider_native'
                        ELSE 'postgresql_row'
                    END,
                    'exact_score_representation', profiles.representation,
                    'hnsw_opclass', CASE profiles.representation || ':' || profiles.metric
                        WHEN 'int8:l2' THEN 'int8vec_hnsw_ops'
                        WHEN 'int8:inner_product' THEN 'int8vec_hnsw_ip_ops'
                        WHEN 'int8:cosine' THEN 'int8vec_hnsw_cosine_ops'
                        WHEN 'int8:l1' THEN 'int8vec_hnsw_l1_ops'
                        WHEN 'uint8:l2' THEN 'uint8vec_hnsw_ops'
                        WHEN 'uint8:inner_product' THEN 'uint8vec_hnsw_ip_ops'
                        WHEN 'uint8:cosine' THEN 'uint8vec_hnsw_cosine_ops'
                        WHEN 'uint8:l1' THEN 'uint8vec_hnsw_l1_ops'
                        WHEN 'bit:hamming' THEN 'bitvec_hnsw_hamming_ops'
                        WHEN 'bit:jaccard' THEN 'bitvec_hnsw_jaccard_ops'
                        ELSE NULL
                    END,
                    'final_score', 'authoritative_source'
                )
           FROM pgcontext._embedding_profiles AS profiles
           JOIN pgcontext._collections AS collections USING (collection_id)
          WHERE collections.collection_name = $1
            AND profiles.profile_name = $2
            AND pg_catalog.pg_has_role(SESSION_USER, collections.owner_role, 'MEMBER')",
        &[collection.as_str().into(), profile_name.as_str().into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to explain embedding profile: {error}"),
        )
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
            format!("embedding profile does not exist: {profile_name}"),
        )
    })
}

pub(crate) fn provider_profile_contract(
    collection: &str,
    profile_name: &str,
) -> ProviderProfileContract {
    let row = Spi::connect(|client| {
        let rows = client.select(
            "SELECT profiles.representation,
                    profiles.dimensions,
                    profiles.bit_order,
                    profiles.byte_order
               FROM pgcontext._embedding_profiles AS profiles
               JOIN pgcontext._collections AS collections USING (collection_id)
              WHERE collections.collection_name = $1
                AND profiles.profile_name = $2
                AND pg_catalog.pg_has_role(SESSION_USER, collections.owner_role, 'MEMBER')
                AND EXISTS (
                    SELECT 1
                      FROM pg_catalog.pg_class AS source_table
                      JOIN pg_catalog.pg_namespace AS source_namespace
                        ON source_namespace.oid = source_table.relnamespace
                      JOIN pg_catalog.pg_attribute AS source_attribute
                        ON source_attribute.attrelid = source_table.oid
                       AND source_attribute.attnum = profiles.source_attnum
                       AND source_attribute.attname = profiles.source_column_name
                       AND source_attribute.atttypmod = profiles.source_typmod
                      JOIN pg_catalog.pg_type AS source_type
                        ON source_type.oid = source_attribute.atttypid
                       AND source_type.typname = profiles.source_type_name
                      JOIN pg_catalog.pg_namespace AS source_type_namespace
                        ON source_type_namespace.oid = source_type.typnamespace
                       AND source_type_namespace.nspname = 'pgcontext'
                      JOIN pg_catalog.pg_class AS index_class
                        ON index_class.relname = profiles.hnsw_index_name
                      JOIN pg_catalog.pg_namespace AS index_namespace
                        ON index_namespace.oid = index_class.relnamespace
                       AND index_namespace.nspname = profiles.hnsw_schema_name
                      JOIN pg_catalog.pg_index AS index_catalog
                        ON index_catalog.indexrelid = index_class.oid
                       AND index_catalog.indrelid = source_table.oid
                       AND index_catalog.indkey[0] = profiles.source_attnum
                       AND index_catalog.indnkeyatts = 1
                       AND index_catalog.indnatts = 1
                       AND index_catalog.indexprs IS NULL
                       AND index_catalog.indpred IS NULL
                       AND index_catalog.indisvalid
                       AND index_catalog.indisready
                       AND index_catalog.indislive
                      JOIN pg_catalog.pg_am AS access_method
                        ON access_method.oid = index_class.relam
                       AND access_method.amname = 'pgcontext_hnsw'
                      JOIN pg_catalog.pg_opclass AS opclass
                        ON opclass.oid = index_catalog.indclass[0]
                       AND opclass.opcname = profiles.hnsw_opclass
                      JOIN pg_catalog.pg_namespace AS opclass_namespace
                        ON opclass_namespace.oid = opclass.opcnamespace
                       AND opclass_namespace.nspname = 'pgcontext'
                     WHERE source_namespace.nspname = profiles.source_schema_name
                       AND source_table.relname = profiles.source_table_name
                       AND source_table.oid = collections.source_table_oid
                )",
            Some(1),
            &[collection.into(), profile_name.into()],
        )?;
        if rows.is_empty() {
            return Ok::<_, spi::Error>(None);
        }
        let row = rows.first();
        Ok::<_, spi::Error>(Some((
            required(row.get::<String>(1)?, "representation"),
            required(row.get::<i32>(2)?, "dimensions"),
            row.get::<String>(3)?,
            row.get::<String>(4)?,
        )))
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to resolve embedding profile contract: {error}"),
        )
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
            format!("embedding profile binding is missing, stale, or inaccessible: {profile_name}"),
        )
    });
    let representation = match row.0.as_str() {
        "dense" => VectorRepresentation::Dense,
        "half" => VectorRepresentation::Half,
        "sparse" => VectorRepresentation::Sparse,
        "bit" => VectorRepresentation::Bit,
        "int8" => VectorRepresentation::Int8,
        "uint8" => VectorRepresentation::UInt8,
        other => invalid_profile(format!("unsupported stored representation: {other}")),
    };
    let dimensions = usize::try_from(row.1)
        .unwrap_or_else(|_| invalid_profile("stored profile dimensions are invalid".to_owned()));
    let binary_layout = match (row.2.as_deref(), row.3.as_deref()) {
        (Some(bit_order), Some(byte_order)) => Some(ProviderBinaryLayout {
            bit_order: parse_bit_order(bit_order).0,
            byte_order: parse_byte_order(byte_order).0,
        }),
        (None, None) => None,
        _ => invalid_profile("stored provider binary layout is incomplete".to_owned()),
    };
    ProviderProfileContract {
        dimensions,
        representation,
        binary_layout,
    }
}

struct ParsedProfile {
    profile: EmbeddingProfile,
    representation: &'static str,
    normalization: &'static str,
    bit_order: Option<&'static str>,
    byte_order: Option<&'static str>,
    configuration_hash: String,
    matryoshka_prefixes: Option<Vec<i32>>,
    version_columns: Option<VersionColumnNames>,
}

impl ParsedProfile {
    fn json(&self) -> Value {
        json!({
            "representation": self.representation,
            "dimensions": self.profile.dimensions(),
            "normalization": self.normalization,
            "metric": distance_metric_label(self.profile.metric()),
            "provider": self.profile.provider(),
            "model": self.profile.model(),
            "revision": self.profile.revision(),
            "input_template": self.profile.input_template(),
            "output_template": self.profile.output_template(),
            "bit_order": self.bit_order,
            "byte_order": self.byte_order,
            "scale": self.profile.integer_scale().map(|scale| scale.scale),
            "zero_point": self.profile.integer_scale().map(|scale| scale.zero_point),
            "configuration_hash": self.configuration_hash,
            "matryoshka_prefixes": self.matryoshka_prefixes,
            "source_version_column": self.version_columns.as_ref().map(|columns| columns.source.as_str()),
            "embedding_version_column": self.version_columns.as_ref().map(|columns| columns.embedding.as_str()),
        })
    }
}

fn parse_profile(profile: &JsonB) -> ParsedProfile {
    let object = profile.0.as_object().unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "embedding profile must be a JSON object",
        )
    });
    reject_unknown_keys(object);
    let representation_label = required_string(object, "representation");
    let (representation, representation_label) = match representation_label {
        "dense" => (VectorRepresentation::Dense, "dense"),
        "half" => (VectorRepresentation::Half, "half"),
        "sparse" => (VectorRepresentation::Sparse, "sparse"),
        "bit" => (VectorRepresentation::Bit, "bit"),
        "int8" => (VectorRepresentation::Int8, "int8"),
        "uint8" => (VectorRepresentation::UInt8, "uint8"),
        other => invalid_profile(format!("unsupported representation: {other}")),
    };
    let dimensions = required_i64(object, "dimensions");
    let dimensions = usize::try_from(dimensions)
        .unwrap_or_else(|_| invalid_profile("dimensions exceed usize".to_owned()));
    let normalization_label = required_string(object, "normalization");
    let (normalization, normalization_label) = match normalization_label {
        "none" => (VectorNormalization::None, "none"),
        "unit_l2" => (VectorNormalization::UnitL2, "unit_l2"),
        other => invalid_profile(format!("unsupported normalization: {other}")),
    };
    let metric_label = required_string(object, "metric");
    let metric = parse_distance_metric(metric_label)
        .unwrap_or_else(|| invalid_profile(format!("unsupported metric: {metric_label}")));
    let bit_order = optional_string(object, "bit_order").map(parse_bit_order);
    let byte_order = optional_string(object, "byte_order").map(parse_byte_order);
    let binary_layout = match (bit_order, byte_order) {
        (Some((bit_order, _)), Some((byte_order, _))) => Some(ProviderBinaryLayout {
            bit_order,
            byte_order,
        }),
        (None, None) => None,
        _ => invalid_profile("bit_order and byte_order must be supplied together".to_owned()),
    };
    let scale = optional_f64(object, "scale");
    let zero_point = optional_i64(object, "zero_point");
    let integer_scale = match (scale, zero_point) {
        (Some(scale), Some(zero_point)) => Some(IntegerScale {
            scale,
            zero_point: i32::try_from(zero_point)
                .unwrap_or_else(|_| invalid_profile("zero_point exceeds int4".to_owned())),
        }),
        (None, None) => None,
        _ => invalid_profile("scale and zero_point must be supplied together".to_owned()),
    };
    let configuration_hash = required_string(object, "configuration_hash").to_owned();
    let hash = u64::from_str_radix(&configuration_hash, 16).unwrap_or_else(|_| {
        invalid_profile("configuration_hash must be 16 lowercase hex digits".to_owned())
    });
    if configuration_hash.len() != 16
        || configuration_hash
            .bytes()
            .any(|byte| byte.is_ascii_uppercase())
    {
        invalid_profile("configuration_hash must be 16 lowercase hex digits".to_owned());
    }
    let Some(profile_id) = ProfileId::new(1) else {
        invalid_profile("internal profile identity is invalid".to_owned())
    };
    let parsed = EmbeddingProfile::new(
        profile_id,
        representation,
        dimensions,
        normalization,
        metric,
        required_string(object, "provider").to_owned(),
        required_string(object, "model").to_owned(),
        required_string(object, "revision").to_owned(),
        required_string(object, "input_template").to_owned(),
        required_string(object, "output_template").to_owned(),
        binary_layout,
        integer_scale,
        hash,
    )
    .unwrap_or_else(|error| invalid_profile(error.to_string()));
    let matryoshka_prefixes = optional_prefixes(object);
    let version_columns = match (
        optional_string(object, "source_version_column"),
        optional_string(object, "embedding_version_column"),
    ) {
        (Some(source), Some(embedding)) => {
            if source.trim().is_empty() || embedding.trim().is_empty() {
                invalid_profile("version column names must not be blank".to_owned());
            }
            if source == embedding {
                invalid_profile(
                    "source_version_column and embedding_version_column must be different"
                        .to_owned(),
                );
            }
            Some(VersionColumnNames {
                source: source.to_owned(),
                embedding: embedding.to_owned(),
            })
        }
        (None, None) => None,
        _ => invalid_profile(
            "source_version_column and embedding_version_column must be supplied together"
                .to_owned(),
        ),
    };
    let parsed = match &matryoshka_prefixes {
        None => parsed,
        Some(prefixes) => {
            let declared = prefixes
                .iter()
                .map(|prefix| {
                    usize::try_from(*prefix)
                        .ok()
                        .and_then(|prefix| PrefixDimensions::new(prefix).ok())
                        .unwrap_or_else(|| {
                            invalid_profile(format!("invalid Matryoshka prefix: {prefix}"))
                        })
                })
                .collect::<Vec<_>>();
            let policy = MatryoshkaPolicy::new(dimensions, declared, normalization)
                .unwrap_or_else(|error| invalid_profile(error.to_string()));
            parsed
                .with_matryoshka(policy)
                .unwrap_or_else(|error| invalid_profile(error.to_string()))
        }
    };
    ParsedProfile {
        profile: parsed,
        representation: representation_label,
        normalization: normalization_label,
        bit_order: bit_order.map(|(_, label)| label),
        byte_order: byte_order.map(|(_, label)| label),
        configuration_hash,
        matryoshka_prefixes,
        version_columns,
    }
}

/// Reads the optional `matryoshka_prefixes` array of positive integers.
///
/// An explicit JSON `null` and an absent key both mean "no policy"; an empty
/// array is rejected, because a declared policy must certify at least one
/// prefix.
fn optional_prefixes(object: &Map<String, Value>) -> Option<Vec<i32>> {
    let value = object.get("matryoshka_prefixes")?;
    if value.is_null() {
        return None;
    }
    let values = value.as_array().unwrap_or_else(|| {
        invalid_profile("matryoshka_prefixes must be an array of positive integers".to_owned())
    });
    if values.is_empty() {
        invalid_profile("matryoshka_prefixes must not be empty".to_owned());
    }
    Some(
        values
            .iter()
            .map(|entry| {
                entry
                    .as_i64()
                    .and_then(|prefix| i32::try_from(prefix).ok())
                    .filter(|prefix| *prefix > 0)
                    .unwrap_or_else(|| {
                        invalid_profile(
                            "matryoshka_prefixes must contain positive integers".to_owned(),
                        )
                    })
            })
            .collect(),
    )
}

/// Validates the lifecycle a profile may be registered into.
///
/// A profile registers into `shadow` (backfilling, not yet answering) or
/// `active` (answering immediately). `draining`, `retired`, and `failed`
/// describe a profile that already served, so they are reachable only through
/// a validated transition, never as an initial state.
fn registration_lifecycle(lifecycle: &str) -> ProfileLifecycle {
    let parsed = ProfileLifecycle::parse(lifecycle)
        .unwrap_or_else(|error| invalid_profile(error.to_string()));
    if matches!(parsed, ProfileLifecycle::Shadow | ProfileLifecycle::Active) {
        return parsed;
    }
    invalid_profile(format!(
        "embedding profiles register into shadow or active, not {}",
        parsed.stable_name()
    ))
}

fn insert_profile(
    collection_id: i64,
    profile_name: &str,
    binding: &ProfileBinding,
    parsed: &ParsedProfile,
    lifecycle: ProfileLifecycle,
) {
    let dimensions = i32::try_from(parsed.profile.dimensions())
        .unwrap_or_else(|_| invalid_profile("dimensions exceed int4".to_owned()));
    let scale = parsed.profile.integer_scale();
    let source_version_name = binding
        .version_columns
        .as_ref()
        .map(|columns| columns.source_name.as_str());
    let source_version_attnum = binding
        .version_columns
        .as_ref()
        .map(|columns| columns.source_attnum);
    let embedding_version_name = binding
        .version_columns
        .as_ref()
        .map(|columns| columns.embedding_name.as_str());
    let embedding_version_attnum = binding
        .version_columns
        .as_ref()
        .map(|columns| columns.embedding_attnum);
    Spi::run_with_args(
        "INSERT INTO pgcontext._embedding_profiles (
             collection_id, profile_name,
             source_schema_name, source_table_name, source_column_name, source_attnum,
             source_type_name, source_typmod,
             source_version_column_name, source_version_attnum,
             embedding_version_column_name, embedding_version_attnum,
             hnsw_schema_name, hnsw_index_name, hnsw_opclass,
             representation, dimensions, normalization, metric,
             provider, model, revision, input_template, output_template, bit_order, byte_order,
             scale, zero_point, configuration_hash, matryoshka_prefixes, lifecycle
         ) VALUES (
             $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,
             $21,$22,$23,$24,$25,$26,$27,$28,$29,$30,$31
         )",
        &[
            collection_id.into(),
            profile_name.into(),
            binding.source_schema_name.as_str().into(),
            binding.source_table_name.as_str().into(),
            binding.source_column_name.as_str().into(),
            binding.source_attnum.into(),
            binding.source_type_name.as_str().into(),
            binding.source_typmod.into(),
            source_version_name.into(),
            source_version_attnum.into(),
            embedding_version_name.into(),
            embedding_version_attnum.into(),
            binding.hnsw_schema_name.as_str().into(),
            binding.hnsw_index_name.as_str().into(),
            binding.hnsw_opclass.as_str().into(),
            parsed.representation.into(),
            dimensions.into(),
            parsed.normalization.into(),
            distance_metric_label(parsed.profile.metric()).into(),
            parsed.profile.provider().into(),
            parsed.profile.model().into(),
            parsed.profile.revision().into(),
            parsed.profile.input_template().into(),
            parsed.profile.output_template().into(),
            parsed.bit_order.into(),
            parsed.byte_order.into(),
            scale.map(|value| value.scale).into(),
            scale.map(|value| value.zero_point).into(),
            parsed.configuration_hash.as_str().into(),
            parsed.matryoshka_prefixes.clone().into(),
            lifecycle.stable_name().into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to register embedding profile: {error}"),
        )
    });
}

fn resolve_profile_binding(
    collection: ProfileCollection,
    collection_name: &str,
    source_column: &str,
    hnsw_index: &str,
    parsed: &ParsedProfile,
) -> ProfileBinding {
    if source_column.trim().is_empty() {
        invalid_profile("source_column must not be empty".to_owned());
    }
    if !hnsw_index.contains('.') {
        invalid_profile("hnsw_index must be schema-qualified".to_owned());
    }
    let expected_type = representation_type_name(parsed.representation);
    let dimensions = i32::try_from(parsed.profile.dimensions())
        .unwrap_or_else(|_| invalid_profile("dimensions exceed int4".to_owned()));
    let source = Spi::connect(|client| {
        let rows = client.select(
            "SELECT source_namespace.nspname::text,
                    source_table.relname::text,
                    source_attribute.attnum,
                    source_type_namespace.nspname::text,
                    source_type.typname::text,
                    source_attribute.atttypmod
               FROM pg_catalog.pg_class AS source_table
               JOIN pg_catalog.pg_namespace AS source_namespace
                 ON source_namespace.oid = source_table.relnamespace
               JOIN pg_catalog.pg_attribute AS source_attribute
                 ON source_attribute.attrelid = source_table.oid
                AND source_attribute.attname = $2
                AND source_attribute.attnum > 0
                AND NOT source_attribute.attisdropped
               JOIN pg_catalog.pg_type AS source_type
                 ON source_type.oid = source_attribute.atttypid
               JOIN pg_catalog.pg_namespace AS source_type_namespace
                 ON source_type_namespace.oid = source_type.typnamespace
              WHERE source_table.oid = $1",
            Some(1),
            &[collection.source_table_oid.into(), source_column.into()],
        )?;
        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
                format!(
                    "source column does not exist for collection {collection_name}: {source_column}"
                ),
            );
        }
        let row = rows.first();
        Ok::<_, spi::Error>((
            required(row.get::<String>(1)?, "source_schema_name"),
            required(row.get::<String>(2)?, "source_table_name"),
            required(row.get::<i16>(3)?, "source_attnum"),
            required(row.get::<String>(4)?, "source_type_schema"),
            required(row.get::<String>(5)?, "source_type_name"),
            required(row.get::<i32>(6)?, "source_typmod"),
        ))
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to validate embedding profile source column: {error}"),
        )
    });
    let (source_schema_name, source_table_name, source_attnum, type_schema, type_name, typmod) =
        source;
    if type_schema != "pgcontext" || type_name != expected_type {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATATYPE_MISMATCH,
            format!(
                "embedding profile representation {} requires pgcontext.{expected_type}, found {type_schema}.{type_name}",
                parsed.representation
            ),
        );
    }
    if typmod != dimensions {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATATYPE_MISMATCH,
            format!(
                "embedding profile dimensions {dimensions} do not match source column typmod {typmod}"
            ),
        );
    }
    let version_columns = parsed.version_columns.as_ref().map(|columns| {
        let source_attnum = resolve_version_column(
            collection.source_table_oid,
            &columns.source,
            "source_version_column",
        );
        let embedding_attnum = resolve_version_column(
            collection.source_table_oid,
            &columns.embedding,
            "embedding_version_column",
        );
        VersionColumnBinding {
            source_name: columns.source.clone(),
            source_attnum,
            embedding_name: columns.embedding.clone(),
            embedding_attnum,
        }
    });

    let expected_opclass = expected_hnsw_opclass(parsed);
    let index = Spi::connect(|client| {
        let rows = client.select(
            "SELECT index_namespace.nspname::text,
                    index_class.relname::text,
                    access_method.amname::text,
                    opclass_namespace.nspname::text,
                    opclass.opcname::text,
                    index_catalog.indrelid,
                    index_catalog.indkey[0],
                    index_catalog.indnkeyatts = 1
                        AND index_catalog.indnatts = 1
                        AND index_catalog.indexprs IS NULL
                        AND index_catalog.indpred IS NULL,
                    index_catalog.indisvalid,
                    index_catalog.indisready,
                    index_catalog.indislive
               FROM pg_catalog.pg_class AS index_class
               JOIN pg_catalog.pg_namespace AS index_namespace
                 ON index_namespace.oid = index_class.relnamespace
               JOIN pg_catalog.pg_index AS index_catalog
                 ON index_catalog.indexrelid = index_class.oid
               JOIN pg_catalog.pg_am AS access_method
                 ON access_method.oid = index_class.relam
               JOIN pg_catalog.pg_opclass AS opclass
                 ON opclass.oid = index_catalog.indclass[0]
               JOIN pg_catalog.pg_namespace AS opclass_namespace
                 ON opclass_namespace.oid = opclass.opcnamespace
              WHERE index_class.oid = pg_catalog.to_regclass($1)",
            Some(1),
            &[hnsw_index.into()],
        )?;
        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                format!("HNSW index does not exist: {hnsw_index}"),
            );
        }
        let row = rows.first();
        Ok::<_, spi::Error>((
            required(row.get::<String>(1)?, "hnsw_schema_name"),
            required(row.get::<String>(2)?, "hnsw_index_name"),
            required(row.get::<String>(3)?, "access_method"),
            required(row.get::<String>(4)?, "opclass_schema"),
            required(row.get::<String>(5)?, "hnsw_opclass"),
            required(row.get::<pg_sys::Oid>(6)?, "index_source_table"),
            required(row.get::<i16>(7)?, "index_source_attnum"),
            required(row.get::<bool>(8)?, "simple_index"),
            required(row.get::<bool>(9)?, "index_valid"),
            required(row.get::<bool>(10)?, "index_ready"),
            required(row.get::<bool>(11)?, "index_live"),
        ))
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to validate embedding profile HNSW index: {error}"),
        )
    });
    let (
        hnsw_schema_name,
        hnsw_index_name,
        access_method,
        opclass_schema,
        hnsw_opclass,
        index_source_table,
        index_source_attnum,
        simple_index,
        index_valid,
        index_ready,
        index_live,
    ) = index;
    if access_method != "pgcontext_hnsw"
        || opclass_schema != "pgcontext"
        || hnsw_opclass != expected_opclass
        || index_source_table != collection.source_table_oid
        || index_source_attnum != source_attnum
        || !simple_index
        || !index_valid
        || !index_ready
        || !index_live
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_WRONG_OBJECT_TYPE,
            format!(
                "index {hnsw_index} is not the live pgcontext.{expected_opclass} binding for {source_schema_name}.{source_table_name}.{source_column}"
            ),
        );
    }

    ProfileBinding {
        source_schema_name,
        source_table_name,
        source_column_name: source_column.to_owned(),
        source_attnum,
        source_type_name: type_name,
        source_typmod: typmod,
        version_columns,
        hnsw_schema_name,
        hnsw_index_name,
        hnsw_opclass,
    }
}

fn resolve_version_column(
    source_table_oid: pg_sys::Oid,
    column_name: &str,
    argument_name: &'static str,
) -> i16 {
    let row = Spi::connect(|client| {
        let rows = client.select(
            "SELECT attribute.attnum, attribute.atttypid
               FROM pg_catalog.pg_attribute AS attribute
              WHERE attribute.attrelid = $1
                AND attribute.attname = $2
                AND attribute.attnum > 0
                AND NOT attribute.attisdropped",
            Some(1),
            &[source_table_oid.into(), column_name.into()],
        )?;
        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
                format!("{argument_name} does not exist: {column_name}"),
            );
        }
        let row = rows.first();
        Ok::<_, spi::Error>((
            required(row.get::<i16>(1)?, "version column attnum"),
            required(row.get::<pg_sys::Oid>(2)?, "version column type"),
        ))
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to validate {argument_name}: {error}"),
        )
    });
    if row.1 != pg_sys::INT8OID {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATATYPE_MISMATCH,
            format!("{argument_name} must be a pg_catalog.int8 column: {column_name}"),
        );
    }
    row.0
}

fn representation_type_name(representation: &str) -> &'static str {
    match representation {
        "dense" => "vector",
        "half" => "halfvec",
        "sparse" => "sparsevec",
        "bit" => "bitvec",
        "int8" => "int8vec",
        "uint8" => "uint8vec",
        _ => invalid_profile(format!("unsupported representation: {representation}")),
    }
}

fn expected_hnsw_opclass(parsed: &ParsedProfile) -> &'static str {
    match (
        parsed.representation,
        distance_metric_label(parsed.profile.metric()),
    ) {
        ("dense", "l2") => "vector_hnsw_ops",
        ("dense", "inner_product") => "vector_hnsw_ip_ops",
        ("dense", "cosine") => "vector_hnsw_cosine_ops",
        ("dense", "l1") => "vector_hnsw_l1_ops",
        ("half", "l2") => "halfvec_hnsw_ops",
        ("half", "inner_product") => "halfvec_hnsw_ip_ops",
        ("half", "cosine") => "halfvec_hnsw_cosine_ops",
        ("half", "l1") => "halfvec_hnsw_l1_ops",
        ("sparse", "l2") => "sparsevec_hnsw_ops",
        ("sparse", "inner_product") => "sparsevec_hnsw_ip_ops",
        ("sparse", "cosine") => "sparsevec_hnsw_cosine_ops",
        ("sparse", "l1") => "sparsevec_hnsw_l1_ops",
        ("bit", "hamming") => "bitvec_hnsw_hamming_ops",
        ("bit", "jaccard") => "bitvec_hnsw_jaccard_ops",
        ("int8", "l2") => "int8vec_hnsw_ops",
        ("int8", "inner_product") => "int8vec_hnsw_ip_ops",
        ("int8", "cosine") => "int8vec_hnsw_cosine_ops",
        ("int8", "l1") => "int8vec_hnsw_l1_ops",
        ("uint8", "l2") => "uint8vec_hnsw_ops",
        ("uint8", "inner_product") => "uint8vec_hnsw_ip_ops",
        ("uint8", "cosine") => "uint8vec_hnsw_cosine_ops",
        ("uint8", "l1") => "uint8vec_hnsw_l1_ops",
        _ => invalid_profile("embedding profile has no compatible HNSW opclass".to_owned()),
    }
}

fn resolve_collection(collection: &str) -> ProfileCollection {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT collection_id, owner_role, source_table_oid
               FROM pgcontext._collections
              WHERE collection_name = $1",
            Some(1),
            &[collection.into()],
        )?;
        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                format!("collection does not exist: {collection}"),
            );
        }
        let row = rows.first();
        Ok::<_, spi::Error>(ProfileCollection {
            collection_id: required(row.get::<i64>(1)?, "collection_id"),
            owner_role: required(row.get::<pg_sys::Oid>(2)?, "owner_role"),
            source_table_oid: required(row.get::<pg_sys::Oid>(3)?, "source_table_oid"),
        })
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("embedding profile collection lookup failed: {error}"),
        )
    })
}

fn require_collection_owner(collection: ProfileCollection, collection_name: &str) {
    let allowed = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.pg_has_role(SESSION_USER, $1::oid, 'MEMBER')",
        &[collection.owner_role.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to check collection ownership: {error}"),
        )
    })
    .unwrap_or_default();
    if !allowed {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            format!("permission denied for collection {collection_name}"),
        );
    }
}

fn reject_duplicate_profile(collection_id: i64, profile_name: &str) {
    let exists = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS (SELECT 1 FROM pgcontext._embedding_profiles WHERE collection_id = $1 AND profile_name = $2)",
        &[collection_id.into(), profile_name.into()],
    ).unwrap_or_else(|error| raise_sql_error(PgSqlErrorCode::ERRCODE_INTERNAL_ERROR, format!("failed to check embedding profile uniqueness: {error}"))).unwrap_or_default();
    if exists {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DUPLICATE_OBJECT,
            format!("embedding profile already exists: {profile_name}"),
        );
    }
}

fn reject_unknown_keys(object: &Map<String, Value>) {
    let allowed = PROFILE_KEYS.into_iter().collect::<BTreeSet<_>>();
    if let Some(key) = object.keys().find(|key| !allowed.contains(key.as_str())) {
        invalid_profile(format!("unsupported embedding profile field: {key}"));
    }
}

fn required_string<'a>(object: &'a Map<String, Value>, key: &str) -> &'a str {
    object
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| invalid_profile(format!("embedding profile {key} must be a string")))
}
fn optional_string<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    match object.get(key) {
        None | Some(Value::Null) => None,
        Some(value) => value.as_str().or_else(|| {
            invalid_profile(format!("embedding profile {key} must be a string or null"))
        }),
    }
}
fn required_i64(object: &Map<String, Value>, key: &str) -> i64 {
    object
        .get(key)
        .and_then(Value::as_i64)
        .unwrap_or_else(|| invalid_profile(format!("embedding profile {key} must be an integer")))
}
fn optional_i64(object: &Map<String, Value>, key: &str) -> Option<i64> {
    match object.get(key) {
        None | Some(Value::Null) => None,
        Some(value) => value.as_i64().or_else(|| {
            invalid_profile(format!(
                "embedding profile {key} must be an integer or null"
            ))
        }),
    }
}
fn optional_f64(object: &Map<String, Value>, key: &str) -> Option<f64> {
    match object.get(key) {
        None | Some(Value::Null) => None,
        Some(value) => value.as_f64().or_else(|| {
            invalid_profile(format!("embedding profile {key} must be numeric or null"))
        }),
    }
}
fn parse_bit_order(value: &str) -> (ProviderBitOrder, &'static str) {
    match value {
        "msb_first" => (ProviderBitOrder::MostSignificantFirst, "msb_first"),
        "lsb_first" => (ProviderBitOrder::LeastSignificantFirst, "lsb_first"),
        other => invalid_profile(format!("unsupported bit_order: {other}")),
    }
}
fn parse_byte_order(value: &str) -> (ProviderByteOrder, &'static str) {
    match value {
        "msb_first" => (ProviderByteOrder::MostSignificantFirst, "msb_first"),
        "lsb_first" => (ProviderByteOrder::LeastSignificantFirst, "lsb_first"),
        other => invalid_profile(format!("unsupported byte_order: {other}")),
    }
}
fn invalid_profile(message: String) -> ! {
    raise_sql_error(PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE, message)
}
fn required<T>(value: Option<T>, label: &str) -> T {
    value.unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            format!("embedding profile {label} is null"),
        )
    })
}

/// Loads the certified Matryoshka policy bound to one registered vector column.
///
/// Returns `None` when the collection has no profile for that column or the
/// profile certifies no prefix. Reads go through the membership-filtered view,
/// so a non-member observes no policy and simply serves full-dimension search.
pub(crate) fn matryoshka_policy_for_column(
    collection_id: i64,
    vector_column_name: &str,
) -> Option<MatryoshkaPolicy> {
    let row = Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT dimensions, normalization, matryoshka_prefixes
                   FROM pgcontext._visible_embedding_profiles
                  WHERE collection_id = $1
                    AND source_column_name = $2
                    AND matryoshka_prefixes IS NOT NULL
                    AND lifecycle IN ('active', 'draining')
                  ORDER BY embedding_profile_id DESC",
                Some(1),
                &[collection_id.into(), vector_column_name.into()],
            )
            .ok()?
            .first();
        let dimensions = rows.get::<i32>(1).ok().flatten()?;
        let normalization = rows.get::<String>(2).ok().flatten()?;
        let prefixes = rows.get::<Vec<Option<i32>>>(3).ok().flatten()?;
        Some((dimensions, normalization, prefixes))
    })?;
    let (dimensions, normalization, prefixes) = row;
    let normalization = match normalization.as_str() {
        "unit_l2" => VectorNormalization::UnitL2,
        _ => VectorNormalization::None,
    };
    let declared = prefixes
        .into_iter()
        .flatten()
        .map(|prefix| {
            usize::try_from(prefix)
                .ok()
                .and_then(|prefix| PrefixDimensions::new(prefix).ok())
        })
        .collect::<Option<Vec<_>>>()?;
    MatryoshkaPolicy::new(usize::try_from(dimensions).ok()?, declared, normalization).ok()
}

/// Moves a registered embedding profile to a new lifecycle state.
///
/// The transition is validated by the typed lifecycle table before any catalog
/// write, so a profile cannot skip backfill, be resurrected from `retired`, or
/// be re-declared into the state it already holds.
///
/// # Panics
///
/// Raises `invalid_parameter_value` for an unknown state or an illegal
/// transition, `undefined_object` when the profile is not registered, and
/// `insufficient_privilege` when the caller does not own the collection.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn set_embedding_profile_lifecycle(
    collection: String,
    profile_name: String,
    lifecycle: String,
) -> String {
    let requested = ProfileLifecycle::parse(&lifecycle)
        .unwrap_or_else(|error| invalid_profile(error.to_string()));
    let collection_id = crate::lexical_catalog::require_collection_owner_id(&collection);

    // An unregistered profile yields an empty result set, and reading a scalar
    // from one is an SPI cursor error rather than a null, so emptiness is
    // checked before any column is read.
    let current = Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT lifecycle
                   FROM pgcontext._visible_embedding_profiles
                  WHERE collection_id = $1 AND profile_name = $2",
                Some(1),
                &[collection_id.into(), profile_name.as_str().into()],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to read embedding profile lifecycle: {error}"),
                )
            });
        rows.into_iter()
            .next()
            .and_then(|row| row.get::<String>(1).ok().flatten())
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
            format!("embedding profile is not registered: {profile_name}"),
        )
    });
    let current = ProfileLifecycle::parse(&current)
        .unwrap_or_else(|error| invalid_profile(error.to_string()));
    let next = current
        .transition_to(requested)
        .unwrap_or_else(|error| invalid_profile(error.to_string()));

    Spi::run_with_args(
        "SELECT pgcontext._set_embedding_profile_lifecycle($1, $2, $3)",
        &[
            collection_id.into(),
            profile_name.as_str().into(),
            next.stable_name().into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to update embedding profile lifecycle: {error}"),
        )
    });
    next.stable_name().to_owned()
}

/// Reports per-profile lifecycle, coverage, and staleness for a collection.
///
/// `covered_points` counts current, version-matched embeddings. `stale_points`
/// counts populated vectors whose required version pair is absent or differs.
/// `active_points` counts only active mappings whose authoritative source row
/// is visible to the invoker under current ACL and RLS policy.
/// Profiles without version bindings retain the single-profile coverage
/// behavior and report zero stale points; they are not eligible for mixed-model
/// execution.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx requires inline name!() table shapes for SQL generation"
)]
pub fn embedding_profile_coverage(
    collection: String,
) -> TableIterator<
    'static,
    (
        name!(profile_name, String),
        name!(lifecycle, String),
        name!(serves_queries, bool),
        name!(source_column, String),
        name!(covered_points, i64),
        name!(stale_points, i64),
        name!(active_points, i64),
    ),
> {
    let collection_id = crate::lexical_catalog::require_collection_owner_id(&collection);
    let profiles = Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT profiles.profile_name, profiles.lifecycle,
                        profiles.source_schema_name, profiles.source_table_name,
                        collections.source_table_oid, source_column_name, source_attnum,
                        source_type_name, source_typmod,
                        source_version_column_name, source_version_attnum,
                        embedding_version_column_name, embedding_version_attnum
                   FROM pgcontext._visible_embedding_profiles AS profiles
                   JOIN pgcontext._visible_collections AS collections
                     ON collections.collection_id = profiles.collection_id
                  WHERE profiles.collection_id = $1
                  ORDER BY profiles.profile_name",
                None,
                &[collection_id.into()],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to list embedding profiles: {error}"),
                )
            });
        let mut profiles = Vec::new();
        for row in rows {
            let read = |index: usize, label: &str| {
                row.get::<String>(index)
                    .unwrap_or_else(|error| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                            format!("failed to read embedding profile {label}: {error}"),
                        )
                    })
                    .unwrap_or_else(|| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                            format!("embedding profile {label} returned null"),
                        )
                    })
            };
            profiles.push((
                read(1, "name"),
                read(2, "lifecycle"),
                read(3, "source schema"),
                read(4, "source table"),
                row.get::<pg_sys::Oid>(5)
                    .unwrap_or_else(|error| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                            format!("failed to read source table OID: {error}"),
                        )
                    })
                    .unwrap_or_else(|| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                            "embedding profile source table OID returned null",
                        )
                    }),
                read(6, "source column"),
                row.get::<i16>(7)
                    .unwrap_or_else(|error| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                            format!("failed to read source column attnum: {error}"),
                        )
                    })
                    .unwrap_or_else(|| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                            "embedding profile source column attnum returned null",
                        )
                    }),
                read(8, "source type"),
                row.get::<i32>(9)
                    .unwrap_or_else(|error| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                            format!("failed to read source vector typmod: {error}"),
                        )
                    })
                    .unwrap_or_else(|| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                            "embedding profile source vector typmod returned null",
                        )
                    }),
                row.get::<String>(10).unwrap_or_else(|error| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                        format!("failed to read source version column: {error}"),
                    )
                }),
                row.get::<i16>(11).unwrap_or_else(|error| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                        format!("failed to read source version attnum: {error}"),
                    )
                }),
                row.get::<String>(12).unwrap_or_else(|error| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                        format!("failed to read embedding version column: {error}"),
                    )
                }),
                row.get::<i16>(13).unwrap_or_else(|error| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                        format!("failed to read embedding version attnum: {error}"),
                    )
                }),
            ));
        }
        profiles
    });

    let mut output = Vec::with_capacity(profiles.len());
    for (
        profile_name,
        lifecycle,
        schema_name,
        table_name,
        table_oid,
        column_name,
        column_attnum,
        source_type_name,
        source_typmod,
        source_version_column,
        source_version_attnum,
        embedding_version_column,
        embedding_version_attnum,
    ) in profiles
    {
        let serves = ProfileLifecycle::parse(&lifecycle)
            .map(ProfileLifecycle::serves_queries)
            .unwrap_or(false);
        let (covered, stale, active) = profile_coverage_counts(
            collection_id,
            &profile_name,
            &schema_name,
            &table_name,
            table_oid,
            &column_name,
            column_attnum,
            &source_type_name,
            source_typmod,
            source_version_column.as_deref(),
            source_version_attnum,
            embedding_version_column.as_deref(),
            embedding_version_attnum,
        );
        output.push((
            profile_name,
            lifecycle,
            serves,
            column_name,
            covered,
            stale,
            active,
        ));
    }
    TableIterator::new(output)
}

#[allow(
    clippy::too_many_arguments,
    reason = "the arguments are the validated catalog binding used to render one bounded count"
)]
fn profile_coverage_counts(
    collection_id: i64,
    profile_name: &str,
    schema_name: &str,
    table_name: &str,
    table_oid: pg_sys::Oid,
    vector_column: &str,
    vector_attnum: i16,
    vector_type_name: &str,
    vector_typmod: i32,
    source_version_column: Option<&str>,
    source_version_attnum: Option<i16>,
    embedding_version_column: Option<&str>,
    embedding_version_attnum: Option<i16>,
) -> (i64, i64, i64) {
    validate_profile_coverage_relation(profile_name, schema_name, table_name, table_oid);
    validate_profile_coverage_attribute(
        profile_name,
        schema_name,
        table_name,
        table_oid,
        CoverageAttributeBinding {
            attnum: vector_attnum,
            name: vector_column,
            type_schema: "pgcontext",
            type_name: vector_type_name,
            typmod: vector_typmod,
        },
    );
    match (
        source_version_column,
        source_version_attnum,
        embedding_version_column,
        embedding_version_attnum,
    ) {
        (Some(source_name), Some(source_attnum), Some(embedding_name), Some(embedding_attnum)) => {
            validate_profile_coverage_attribute(
                profile_name,
                schema_name,
                table_name,
                table_oid,
                CoverageAttributeBinding {
                    attnum: source_attnum,
                    name: source_name,
                    type_schema: "pg_catalog",
                    type_name: "int8",
                    typmod: -1,
                },
            );
            validate_profile_coverage_attribute(
                profile_name,
                schema_name,
                table_name,
                table_oid,
                CoverageAttributeBinding {
                    attnum: embedding_attnum,
                    name: embedding_name,
                    type_schema: "pg_catalog",
                    type_name: "int8",
                    typmod: -1,
                },
            );
        }
        (None, None, None, None) => {}
        _ => raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            format!("embedding profile version binding is incomplete: {profile_name}"),
        ),
    }
    let table = quote_qualified_identifier(schema_name, table_name);
    let vector = quote_identifier(vector_column);
    let (covered_predicate, stale_predicate) =
        match (source_version_column, embedding_version_column) {
            (Some(source_version), Some(embedding_version)) => {
                let source_version = quote_identifier(source_version);
                let embedding_version = quote_identifier(embedding_version);
                (
                    format!(
                        "source.{vector} IS NOT NULL
                     AND source.{source_version} IS NOT NULL
                     AND source.{embedding_version} IS NOT NULL
                     AND source.{source_version} = source.{embedding_version}"
                    ),
                    format!(
                        "source.{vector} IS NOT NULL
                     AND (source.{source_version} IS NULL
                          OR source.{embedding_version} IS NULL
                          OR source.{source_version} <> source.{embedding_version})"
                    ),
                )
            }
            (None, None) => (format!("source.{vector} IS NOT NULL"), "false".to_owned()),
            _ => raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                format!("embedding profile version binding is incomplete: {profile_name}"),
            ),
        };
    Spi::connect(|client| {
        let rows = client.select(
            &format!(
                "SELECT count(*) FILTER (WHERE {covered_predicate}),
                        count(*) FILTER (WHERE {stale_predicate}),
                        count(*)
                   FROM pgcontext._visible_collection_points AS points
                   JOIN {table} AS source ON source.id::text = points.source_key
                  WHERE points.collection_id = $1
                    AND points.deleted_at IS NULL"
            ),
            Some(1),
            &[collection_id.into()],
        )?;
        let row = rows.first();
        Ok::<_, spi::Error>((
            required(row.get::<i64>(1)?, "covered_points"),
            required(row.get::<i64>(2)?, "stale_points"),
            required(row.get::<i64>(3)?, "active_points"),
        ))
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to count embedding profile coverage for {profile_name}: {error}"),
        )
    })
}

fn validate_profile_coverage_relation(
    profile_name: &str,
    schema_name: &str,
    table_name: &str,
    table_oid: pg_sys::Oid,
) {
    let (current_oid, has_select) = Spi::connect(|client| {
        let rows = client.select(
            "SELECT pg_catalog.to_regclass(pg_catalog.format('%I.%I', $1, $2)),
                    pg_catalog.has_table_privilege(SESSION_USER, $3, 'SELECT')",
            Some(1),
            &[schema_name.into(), table_name.into(), table_oid.into()],
        )?;
        let row = rows.first();
        Ok::<_, spi::Error>((row.get::<pg_sys::Oid>(1)?, row.get::<bool>(2)?))
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to validate embedding profile source relation: {error}"),
        )
    });
    let Some(current_oid) = current_oid else {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_TABLE,
            format!("relation \"{schema_name}.{table_name}\" does not exist"),
        );
    };
    if current_oid != table_oid {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            format!("embedding profile source relation identity changed: {profile_name}"),
        );
    }
    if !has_select.unwrap_or(false) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            format!("permission denied for source table of embedding profile {profile_name}"),
        );
    }
}

struct CoverageAttributeBinding<'a> {
    attnum: i16,
    name: &'a str,
    type_schema: &'a str,
    type_name: &'a str,
    typmod: i32,
}

fn validate_profile_coverage_attribute(
    profile_name: &str,
    schema_name: &str,
    table_name: &str,
    table_oid: pg_sys::Oid,
    expected: CoverageAttributeBinding<'_>,
) {
    let expected_type_oid = Spi::get_one_with_args::<pg_sys::Oid>(
        "SELECT pg_catalog.to_regtype(pg_catalog.format('%I.%I', $1, $2))",
        &[expected.type_schema.into(), expected.type_name.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to resolve embedding profile source type: {error}"),
        )
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            format!(
                "embedding profile source type is unavailable: {}.{}",
                expected.type_schema, expected.type_name
            ),
        )
    });
    let actual = Spi::connect(|client| {
        let rows = client.select(
            "SELECT attname::pg_catalog.text, atttypid, atttypmod
               FROM pg_catalog.pg_attribute
              WHERE attrelid = $1 AND attnum = $2 AND NOT attisdropped",
            Some(1),
            &[table_oid.into(), expected.attnum.into()],
        )?;
        if rows.is_empty() {
            return Ok::<_, spi::Error>(None);
        }
        let row = rows.first();
        Ok(Some((
            required(row.get::<String>(1)?, "source column name"),
            required(row.get::<pg_sys::Oid>(2)?, "source column type"),
            required(row.get::<i32>(3)?, "source column typmod"),
        )))
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to validate embedding profile source column: {error}"),
        )
    });
    let Some((actual_name, actual_type_oid, actual_typmod)) = actual else {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
            format!(
                "column \"{}\" of relation \"{schema_name}.{table_name}\" does not exist",
                expected.name
            ),
        );
    };
    if actual_name != expected.name
        || actual_type_oid != expected_type_oid
        || actual_typmod != expected.typmod
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            format!("embedding profile source column identity changed: {profile_name}"),
        );
    }
}
