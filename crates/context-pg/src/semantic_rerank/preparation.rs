fn validate_source_name(name: &str) {
    if name.len() > MAX_RERANK_SOURCE_NAME_BYTES
        || name.trim().is_empty()
        || name.chars().any(char::is_control)
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "semantic rerank source name is invalid",
        );
    }
}

fn positive_i64(value: i64, field: &'static str) -> u64 {
    u64::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!("{field} must be positive"),
            )
        })
}

fn preflight_json(value: &Value, max_bytes: usize, label: &'static str) {
    let mut stack = vec![(value, 1_usize)];
    let mut nodes = 0_usize;
    let mut bytes = 0_usize;
    while let Some((value, depth)) = stack.pop() {
        if depth > context_query::MAX_QUERY_DEPTH {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                format!("{label} nesting is too deep"),
            );
        }
        nodes = nodes.saturating_add(1);
        if nodes > MAX_RERANK_JSON_NODES {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                format!("{label} has too many values"),
            );
        }
        bytes = bytes.saturating_add(size_of::<Value>());
        match value {
            Value::String(value) => {
                bytes = bytes.saturating_add(value.len());
            }
            Value::Array(values) => {
                stack.extend(values.iter().map(|value| (value, depth + 1)));
            }
            Value::Object(values) => {
                for (key, value) in values {
                    bytes = bytes.saturating_add(key.len());
                    stack.push((value, depth + 1));
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
        if bytes > max_bytes || bytes > DEFAULT_QUERY_MEMORY_BYTES {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                format!("{label} exceeds the memory budget"),
            );
        }
    }
}

fn parse_candidate_inputs(value: Value) -> Vec<CandidateInput> {
    let Value::Array(values) = value else {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "semantic rerank candidates must be an array",
        );
    };
    if values.is_empty() || values.len() > MAX_RERANK_CANDIDATES {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "semantic rerank candidate count is outside 1..=512",
        );
    }
    let inputs = values
        .into_iter()
        .map(|value| {
            serde_json::from_value(value).unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                    "semantic rerank candidate is malformed",
                )
            })
        })
        .collect::<Vec<CandidateInput>>();
    let mut occurrences = BTreeSet::new();
    let mut points = BTreeSet::new();
    for input in &inputs {
        if input.occurrence_id == 0
            || input.point_id == 0
            || input.fused_rank == 0
            || !input.fused_score.is_finite()
            || !occurrences.insert(input.occurrence_id)
            || !points.insert(input.point_id)
        {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                "semantic rerank candidate identity or score is invalid",
            );
        }
    }
    inputs
}

fn candidate_metadata_keys(inputs: &[CandidateInput]) -> BTreeSet<String> {
    let mut all_keys = BTreeSet::new();
    for input in inputs {
        let mut candidate_keys = BTreeSet::new();
        for metadata in &input.metadata {
            RerankMetadata::validate(&metadata.key, &metadata.value)
                .unwrap_or_else(|error| raise_query_error(error));
            if !candidate_keys.insert(metadata.key.as_str()) {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                    "semantic rerank candidate metadata repeats a key",
                );
            }
            all_keys.insert(metadata.key.clone());
        }
    }
    if all_keys.len() > MAX_RERANK_METADATA_ENTRIES {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "semantic rerank request references more than eight metadata fields",
        );
    }
    all_keys
}

fn parse_filter(value: Option<&Value>) -> Option<Filter> {
    value.map(|value| {
        let json = serde_json::to_string(value).unwrap_or_else(|_| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                "semantic rerank filter is invalid",
            )
        });
        parse_filter_json(&json).unwrap_or_else(|_| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                "semantic rerank filter is invalid",
            )
        })
    })
}

fn refresh_source(collection_id: i64, source_name: &str) {
    Spi::run_with_args(
        "SELECT pgcontext._refresh_semantic_rerank_source($1, $2)",
        &[collection_id.into(), source_name.into()],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "failed to refresh semantic rerank source",
        )
    });
}

fn load_prepared_source(collection_id: i64, source_name: &str) -> PreparedSource {
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT sources.rerank_source_id,
                        sources.registration_revision,
                        sources.source_table_oid,
                        sources.source_schema_name,
                        sources.source_table_name,
                        sources.source_key_type_schema,
                        sources.source_key_type_name,
                        sources.text_column_name,
                        sources.source_version_column_name,
                        sources.content_hash_rule,
                        sources.status,
                        pg_catalog.has_table_privilege(
                            SESSION_USER, sources.source_table_oid, 'SELECT'
                        )
                   FROM pgcontext._visible_semantic_rerank_sources AS sources
                  WHERE sources.collection_id = $1
                    AND sources.source_name = $2",
                Some(1),
                &[collection_id.into(), source_name.into()],
            )
            .unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    "failed to load semantic rerank source",
                )
            });
        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                "semantic rerank source does not exist",
            );
        }
        let row = rows.first();
        let status = required_first_column::<String>(&row, 11, "source status");
        if status != "ready" {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                "semantic rerank source is stale",
            );
        }
        if !required_first_column::<bool>(&row, 12, "source select privilege") {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
                "permission denied for semantic rerank source",
            );
        }
        PreparedSource {
            source_id: required_first_column(&row, 1, "source identity"),
            registration_revision: required_first_column(&row, 2, "registration revision"),
            source_table_oid: required_first_column(&row, 3, "source table identity"),
            schema_name: required_first_column(&row, 4, "source schema"),
            table_name: required_first_column(&row, 5, "source table"),
            source_key_type_schema: required_first_column(&row, 6, "source key type schema"),
            source_key_type_name: required_first_column(&row, 7, "source key type name"),
            text_column: required_first_column(&row, 8, "text column"),
            version_column: required_first_column(&row, 9, "source version column"),
            content_hash_rule: ContentHashRule::parse(&required_first_column::<String>(
                &row,
                10,
                "content hash rule",
            ))
            .unwrap_or_else(|| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    "semantic rerank content hash rule is invalid",
                )
            }),
        }
    })
}

fn lock_prepared_source(source: &PreparedSource) {
    let source_table = quote_qualified_identifier(&source.schema_name, &source.table_name);
    Spi::run(&format!("LOCK TABLE {source_table} IN ACCESS SHARE MODE")).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "semantic rerank source relation changed during authorization",
        )
    });
    let current_oid = Spi::get_one_with_args::<pg_sys::Oid>(
        "SELECT pg_catalog.to_regclass($1)::oid",
        &[source_table.as_str().into()],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "failed to verify semantic rerank source relation identity",
        )
    });
    if current_oid != Some(source.source_table_oid) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "semantic rerank source relation changed during authorization",
        );
    }
}

fn load_locked_prepared_source(collection_id: i64, source_name: &str) -> PreparedSource {
    refresh_source(collection_id, source_name);
    let before_lock = load_prepared_source(collection_id, source_name);
    lock_prepared_source(&before_lock);
    refresh_source(collection_id, source_name);
    let locked = load_prepared_source(collection_id, source_name);
    if locked != before_lock {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "semantic rerank source registration changed during authorization",
        );
    }
    locked
}

fn load_bounded_filter_fields(
    collection_id: i64,
    source: &PreparedSource,
    filter: Option<&Filter>,
    metadata_keys: &BTreeSet<String>,
) -> LoadedFilterFields {
    if filter.is_none() && metadata_keys.is_empty() {
        return LoadedFilterFields {
            fields: Vec::new(),
            binding_sha256: empty_filter_binding_digest(),
        };
    }
    let mut keys = metadata_keys.clone();
    if let Some(filter) = filter {
        keys.extend(
            filter
                .field_keys()
                .into_iter()
                .map(|key| key.as_str().to_owned()),
        );
    }
    let keys = keys.into_iter().collect::<Vec<_>>();
    let limit = i64::try_from(keys.len().saturating_add(1)).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "semantic rerank filter field count overflow",
        )
    });
    let fields = Spi::connect(|client| {
        let rows = client
            .select(
                &format!(
                    "SELECT fields.filter_key,
                            fields.column_name,
                            fields.source_table_oid,
                            fields.column_attnum,
                            attribute.atttypid,
                            attribute.atttypmod,
                            attribute.attcollation,
                            coalesce(pg_catalog.cardinality(fields.jsonb_path), 0),
                            bounds.path_bytes,
                            CASE WHEN coalesce(pg_catalog.cardinality(fields.jsonb_path), 0) <= {}
                                      AND bounds.path_bytes <= {}
                                 THEN fields.jsonb_path
                            END
                       FROM pgcontext._visible_collection_payload_columns AS fields
                       JOIN pg_catalog.pg_attribute AS attribute
                         ON attribute.attrelid = fields.source_table_oid
                        AND attribute.attnum = fields.column_attnum
                        AND attribute.attname = fields.column_name
                        AND attribute.attnum > 0
                        AND NOT attribute.attisdropped
                        AND (fields.jsonb_path IS NULL OR
                             attribute.atttypid = 'pg_catalog.jsonb'::pg_catalog.regtype)
                       CROSS JOIN LATERAL (
                           SELECT CASE
                               WHEN coalesce(pg_catalog.cardinality(fields.jsonb_path), 0) <= {}
                               THEN coalesce((
                                   SELECT pg_catalog.sum(pg_catalog.octet_length(segment))
                                     FROM pg_catalog.unnest(fields.jsonb_path) AS segment
                               ), 0)::bigint
                               ELSE {}::bigint
                           END AS path_bytes
                       ) AS bounds
                      WHERE fields.collection_id = $1
                        AND fields.filter_key = ANY($2::text[])
                        AND fields.source_table_oid = $3
                      ORDER BY fields.filter_key",
                    context_core::policy::MAX_FILTER_PATH_DEPTH,
                    context_core::policy::MAX_FILTER_PATH_BYTES,
                    context_core::policy::MAX_FILTER_PATH_DEPTH,
                    context_core::policy::MAX_FILTER_PATH_BYTES + 1,
                ),
                Some(limit),
                &[
                    collection_id.into(),
                    keys.clone().into(),
                    source.source_table_oid.into(),
                ],
            )
            .unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    "failed to load semantic rerank filter fields",
                )
            });
        let mut fields = Vec::with_capacity(rows.len().min(keys.len()));
        let mut binding_hasher = filter_binding_hasher();
        for row in rows {
            let source_table_oid =
                required_column::<pg_sys::Oid>(&row, 3, "filter source relation");
            let column_attnum = required_column::<i16>(&row, 4, "filter column number");
            let column_type_oid = required_column::<pg_sys::Oid>(&row, 5, "filter column type");
            let column_typmod = required_column::<i32>(&row, 6, "filter column typmod");
            let column_collation_oid =
                required_column::<pg_sys::Oid>(&row, 7, "filter column collation");
            let depth = usize::try_from(required_column::<i32>(&row, 8, "filter path depth"))
                .unwrap_or(usize::MAX);
            let path_bytes = usize::try_from(required_column::<i64>(&row, 9, "filter path bytes"))
                .unwrap_or(usize::MAX);
            if depth > context_core::policy::MAX_FILTER_PATH_DEPTH
                || path_bytes > context_core::policy::MAX_FILTER_PATH_BYTES
            {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    "semantic rerank filter field exceeds the registered path budget",
                );
            }
            let path = row.get::<Vec<String>>(10).unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    "failed to read semantic rerank filter path",
                )
            });
            let filter_key = required_column(&row, 1, "filter key");
            let column_name = required_column(&row, 2, "filter column");
            fields.push(FilterField {
                filter_key,
                column_name,
                jsonb_path: path,
            });
            let field = fields.last().unwrap_or_else(|| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    "semantic rerank filter binding was not retained",
                )
            });
            hash_filter_binding(
                &mut binding_hasher,
                field,
                source_table_oid,
                column_attnum,
                column_type_oid,
                column_typmod,
                column_collation_oid,
            );
        }
        (fields, binding_hasher.finalize().into())
    });
    if fields.0.len() != keys.len() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
            "semantic rerank filter references an unknown field",
        );
    }
    LoadedFilterFields {
        fields: fields.0,
        binding_sha256: fields.1,
    }
}

fn empty_filter_binding_digest() -> [u8; RERANK_CONTENT_DIGEST_BYTES] {
    let hasher = filter_binding_hasher();
    hasher.finalize().into()
}

fn filter_binding_hasher() -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(b"pgcontext_semantic_rerank_filter_bindings_v2\0");
    hasher
}

fn hash_filter_binding(
    hasher: &mut Sha256,
    field: &FilterField,
    source_table_oid: pg_sys::Oid,
    column_attnum: i16,
    column_type_oid: pg_sys::Oid,
    column_typmod: i32,
    column_collation_oid: pg_sys::Oid,
) {
    hash_binding_bytes(hasher, field.filter_key.as_bytes());
    hash_binding_bytes(hasher, field.column_name.as_bytes());
    hasher.update(source_table_oid.to_u32().to_be_bytes());
    hasher.update(column_attnum.to_be_bytes());
    hasher.update(column_type_oid.to_u32().to_be_bytes());
    hasher.update(column_typmod.to_be_bytes());
    hasher.update(column_collation_oid.to_u32().to_be_bytes());
    match field.jsonb_path.as_deref() {
        None => hasher.update(0_u32.to_be_bytes()),
        Some(path) => {
            hasher.update(u32::try_from(path.len()).unwrap_or(u32::MAX).to_be_bytes());
            for segment in path {
                hash_binding_bytes(hasher, segment.as_bytes());
            }
        }
    }
}

fn hash_binding_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(bytes);
}

fn hydrate_candidates(
    collection_id: i64,
    source: &PreparedSource,
    inputs: &[CandidateInput],
    filter: Option<&Filter>,
    fields: &[FilterField],
    metadata_keys: &BTreeSet<String>,
) -> BTreeMap<u64, HydratedSource> {
    let point_ids = inputs
        .iter()
        .map(|input| input.point_id)
        .collect::<Vec<_>>();
    hydrate_point_ids(
        collection_id,
        source,
        &point_ids,
        filter,
        fields,
        metadata_keys,
    )
}

fn metadata_value_sql(field: &FilterField) -> String {
    let column = format!("source.{}", quote_identifier(&field.column_name));
    match field.jsonb_path.as_deref() {
        None => format!("({column})::pg_catalog.text"),
        Some(path) => {
            let path = path
                .iter()
                .map(|segment| quote_literal(segment))
                .collect::<Vec<_>>()
                .join(", ");
            format!("({column} OPERATOR(pg_catalog.#>>) ARRAY[{path}]::pg_catalog.text[]")
        }
    }
}

fn hydrate_point_ids(
    collection_id: i64,
    source: &PreparedSource,
    point_ids: &[u64],
    filter: Option<&Filter>,
    fields: &[FilterField],
    metadata_keys: &BTreeSet<String>,
) -> BTreeMap<u64, HydratedSource> {
    let point_ids = point_ids
        .iter()
        .copied()
        .map(i64::try_from)
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|_| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                "semantic rerank point identity is outside the SQL domain",
            )
        });
    let source_table = quote_qualified_identifier(&source.schema_name, &source.table_name);
    let key_type =
        quote_qualified_identifier(&source.source_key_type_schema, &source.source_key_type_name);
    let text_column = quote_identifier(&source.text_column);
    let version_column = quote_identifier(&source.version_column);
    let filter_plan = filter.map(|filter| {
        resolve_typed_filter_plan(fields, filter, 2).unwrap_or_else(|_| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                "semantic rerank filter is invalid",
            )
        })
    });
    let predicate = filter_plan
        .as_ref()
        .map_or_else(|| "TRUE".to_owned(), |plan| plan.sql.clone());
    let metadata_fields = metadata_keys
        .iter()
        .map(|key| {
            fields
                .iter()
                .find(|field| field.filter_key == *key)
                .unwrap_or_else(|| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                        "semantic rerank metadata field lost its registered binding",
                    )
                })
        })
        .collect::<Vec<_>>();
    let metadata_projection = metadata_fields
        .iter()
        .map(|field| {
            let expression = metadata_value_sql(field);
            format!(
                ", pg_catalog.octet_length({expression}),
                   CASE WHEN pg_catalog.octet_length({expression}) <= {MAX_RERANK_METADATA_BYTES}
                        THEN {expression}
                   END"
            )
        })
        .collect::<String>();
    let sql = format!(
        "WITH requested AS (
             SELECT point_id, ordinality
               FROM pg_catalog.unnest($1::bigint[]) WITH ORDINALITY AS input(point_id, ordinality)
         )
         SELECT points.point_id,
                pg_catalog.octet_length(source.{text_column}),
                CASE WHEN pg_catalog.octet_length(source.{text_column}) <= {MAX_RERANK_TEXT_BYTES}
                     THEN source.{text_column}
                END,
                source.{version_column}
                {metadata_projection}
           FROM requested
           JOIN pgcontext._visible_collection_points AS points
             ON points.collection_id = $2
            AND points.point_id = requested.point_id
            AND points.deleted_at IS NULL
           JOIN {source_table} AS source
             ON source.id = points.source_key::{key_type}
          WHERE ({predicate})
          ORDER BY requested.ordinality"
    );
    let row_limit = i64::try_from(point_ids.len().saturating_add(1)).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "semantic rerank row limit overflow",
        )
    });
    let mut args: Vec<DatumWithOid<'_>> = vec![point_ids.into(), collection_id.into()];
    if let Some(plan) = filter_plan.as_ref() {
        push_filter_parameter_args(&mut args, &plan.parameters);
    }
    Spi::connect(|client| {
        let rows = client
            .select(&sql, Some(row_limit), &args)
            .unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    "failed to authorize semantic rerank candidates",
                )
            });
        let mut hydrated = BTreeMap::new();
        for row in rows {
            let point_id = required_column::<i64>(&row, 1, "point identity");
            let text_bytes = usize::try_from(required_column::<i32>(&row, 2, "text bytes"))
                .unwrap_or(usize::MAX);
            if text_bytes > MAX_RERANK_TEXT_BYTES {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    "semantic rerank authorized text exceeds 32768 bytes",
                );
            }
            let text = required_column::<String>(&row, 3, "authorized text");
            let version = required_column::<i64>(&row, 4, "source version");
            let version = u64::try_from(version)
                .ok()
                .and_then(SourceVersion::new)
                .unwrap_or_else(|| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                        "semantic rerank source version is invalid",
                    )
                });
            let point = u64::try_from(point_id).unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    "semantic rerank point identity is invalid",
                )
            });
            let mut metadata = BTreeMap::new();
            for (index, key) in metadata_keys.iter().enumerate() {
                let length_ordinal = 5 + index * 2;
                let value_ordinal = length_ordinal + 1;
                let length = optional_column::<i32>(&row, length_ordinal, "metadata bytes")
                    .map(|length| usize::try_from(length).unwrap_or(usize::MAX));
                if length.is_some_and(|length| length > MAX_RERANK_METADATA_BYTES) {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                        "semantic rerank authorized metadata exceeds 1024 bytes",
                    );
                }
                if let Some(value) =
                    optional_column::<String>(&row, value_ordinal, "metadata value")
                {
                    metadata.insert(key.clone(), value);
                }
            }
            if hydrated
                .insert(
                    point,
                    HydratedSource {
                        source_version: version,
                        text,
                        metadata,
                    },
                )
                .is_some()
            {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    "semantic rerank source returned a duplicate point",
                );
            }
        }
        hydrated
    })
}

fn build_candidates(
    inputs: Vec<CandidateInput>,
    mut hydrated: BTreeMap<u64, HydratedSource>,
    content_hash_rule: ContentHashRule,
) -> Vec<RerankCandidate> {
    if hydrated.len() != inputs.len() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            "one or more semantic rerank candidates are not visible",
        );
    }
    inputs
        .into_iter()
        .map(|input| {
            let source = hydrated.remove(&input.point_id).unwrap_or_else(|| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
                    "one or more semantic rerank candidates are not visible",
                )
            });
            let content_digest = content_hash_rule.digest(&source.text);
            let contributions = input
                .contributions
                .into_iter()
                .map(|contribution| {
                    RerankContribution::new(
                        contribution.profile,
                        contribution.rank,
                        contribution.native_score,
                        contribution.weight,
                        contribution.contribution,
                    )
                    .unwrap_or_else(|error| raise_query_error(error))
                })
                .collect::<Vec<_>>();
            for metadata in &input.metadata {
                if source.metadata.get(&metadata.key) != Some(&metadata.value) {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                        "semantic rerank metadata does not match the authoritative source",
                    );
                }
            }
            let metadata = input
                .metadata
                .into_iter()
                .map(|metadata| {
                    RerankMetadata::new(metadata.key, metadata.value)
                        .unwrap_or_else(|error| raise_query_error(error))
                })
                .collect::<Vec<_>>();
            RerankCandidate::new(
                OccurrenceId::new(input.occurrence_id).unwrap_or_else(|| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                        "semantic rerank occurrence identity is invalid",
                    )
                }),
                PointId::new(input.point_id),
                source.source_version,
                RerankContentDigest::new(content_digest),
                source.text,
                input.fused_rank,
                input.fused_score,
                contributions,
                metadata,
            )
            .unwrap_or_else(|error| raise_query_error(error))
        })
        .collect()
}

fn postgres_now_micros() -> i64 {
    Spi::get_one::<i64>(
        "SELECT pg_catalog.floor(
             pg_catalog.extract('epoch', pg_catalog.clock_timestamp()) * 1000000
         )::bigint",
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "failed to read semantic rerank clock",
        )
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "semantic rerank clock returned no value",
        )
    })
}

#[allow(clippy::too_many_arguments)]
fn insert_request(
    collection_id: i64,
    source: &PreparedSource,
    filter_binding_sha256: &[u8; RERANK_CONTENT_DIGEST_BYTES],
    model: &RerankModelName,
    model_revision: u64,
    query: &RerankQuery,
    filter: Option<Value>,
    policy: FailurePolicy,
    allow_partial: bool,
    expires_at_micros: i64,
    candidates: Value,
) -> i64 {
    let filter = JsonB(filter.unwrap_or(Value::Null));
    let model_revision = i64::try_from(model_revision).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "semantic rerank model revision is outside the SQL domain",
        )
    });
    Spi::get_one_with_args::<i64>(
        "SELECT pgcontext._insert_semantic_rerank_request(
             $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12
         )",
        &[
            collection_id.into(),
            source.source_id.into(),
            source.registration_revision.into(),
            filter_binding_sha256.as_slice().into(),
            model.as_str().into(),
            model_revision.into(),
            query.as_str().into(),
            filter.into(),
            policy.as_str().into(),
            allow_partial.into(),
            expires_at_micros.into(),
            JsonB(candidates).into(),
        ],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "failed to persist semantic rerank request",
        )
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "semantic rerank request returned no identity",
        )
    })
}

fn stored_candidates_json(request: &RerankRequest) -> Value {
    Value::Array(
        request
            .candidates()
            .iter()
            .map(|candidate| {
                json!({
                    "occurrence_id": candidate.occurrence_id().get(),
                    "point_id": candidate.point_id().get(),
                    "source_version": candidate.source_version().get(),
                    "content_digest": hex_digest(candidate.content_digest().as_bytes()),
                    "fused_rank": candidate.fused_rank(),
                    "fused_score": candidate.fused_score(),
                    "contributions": contributions_json(candidate),
                    "metadata": metadata_json(candidate),
                })
            })
            .collect(),
    )
}

fn envelope_json(request: &RerankRequest) -> Value {
    json!({
        "version": RERANK_ENVELOPE_VERSION,
        "request_id": request.request_id().get(),
        "model": request.model().as_str(),
        "model_revision": request.model_revision(),
        "expires_at_micros": request.expires_at_micros(),
        "query": request.query().as_str(),
        "candidates": request.candidates().iter().map(|candidate| json!({
            "occurrence_id": candidate.occurrence_id().get(),
            "point_id": candidate.point_id().get(),
            "source_version": candidate.source_version().get(),
            "content_digest": candidate.content_digest().as_bytes(),
            "text": candidate.text(),
            "fused_rank": candidate.fused_rank(),
            "fused_score": candidate.fused_score(),
            "contributions": contributions_json(candidate),
            "metadata": metadata_json(candidate),
        })).collect::<Vec<_>>()
    })
}

struct BoundedCountingWriter {
    bytes: usize,
}

impl Write for BoundedCountingWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(buffer.len())
            .ok_or_else(|| std::io::Error::other("rerank wire byte overflow"))?;
        // Reserve for the durable request identity, whose decimal form can be
        // longer than the provisional identity used during admission.
        if self.bytes > MAX_RERANK_WIRE_BYTES.saturating_sub(32) {
            return Err(std::io::Error::other("rerank wire byte ceiling"));
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn validate_encoded_envelope(request: &RerankRequest) {
    let value = envelope_json(request);
    let mut writer = BoundedCountingWriter { bytes: 0 };
    if serde_json::to_writer(&mut writer, &value).is_err() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "semantic rerank envelope exceeds the encoded wire byte budget",
        );
    }
}

fn contributions_json(candidate: &RerankCandidate) -> Value {
    Value::Array(
        candidate
            .contributions()
            .iter()
            .map(|contribution| {
                json!({
                    "profile": contribution.profile(),
                    "rank": contribution.rank(),
                    "native_score": contribution.native_score(),
                    "weight": contribution.weight(),
                    "contribution": contribution.contribution(),
                })
            })
            .collect(),
    )
}

fn metadata_json(candidate: &RerankCandidate) -> Value {
    Value::Array(
        candidate
            .metadata()
            .iter()
            .map(|metadata| json!({"key": metadata.key(), "value": metadata.value()}))
            .collect(),
    )
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}
