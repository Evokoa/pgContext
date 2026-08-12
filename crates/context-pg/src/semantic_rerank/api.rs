/// Registers a text/version binding used for authorized semantic reranking.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn register_semantic_rerank_source(
    collection: BoundedCollectionName,
    source_name: BoundedRerankSourceName,
    text_column: BoundedColumnName,
    source_version_column: BoundedColumnName,
) -> i64 {
    let collection = collection.0;
    let source_name = source_name.0;
    let text_column = text_column.0;
    let source_version_column = source_version_column.0;
    validate_source_name(&source_name);
    let collection_id = require_collection_owner_id(&collection);
    validate_semantic_rerank_registration_binding(
        collection_id,
        &text_column,
        &source_version_column,
    );
    Spi::get_one_with_args::<i64>(
        "SELECT pgcontext._register_semantic_rerank_source($1, $2, $3, $4)",
        &[
            collection_id.into(),
            source_name.as_str().into(),
            text_column.as_str().into(),
            source_version_column.as_str().into(),
        ],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "failed to register semantic rerank source",
        )
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "semantic rerank source registration returned no identity",
        )
    })
}

fn validate_semantic_rerank_registration_binding(
    collection_id: i64,
    text_column: &str,
    source_version_column: &str,
) {
    let (has_select, text_is_valid, version_is_valid) = Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT pg_catalog.has_table_privilege(
                            SESSION_USER, source_class.oid, 'SELECT'
                        ),
                        text_attribute.atttypid = 'pg_catalog.text'::pg_catalog.regtype,
                        version_attribute.atttypid = 'pg_catalog.int8'::pg_catalog.regtype
                   FROM pgcontext._visible_collections AS collections
                   JOIN pg_catalog.pg_namespace AS source_namespace
                     ON source_namespace.nspname = collections.source_schema_name
                   JOIN pg_catalog.pg_class AS source_class
                     ON source_class.relnamespace = source_namespace.oid
                    AND source_class.relname = collections.source_table_name
                    AND source_class.relkind IN ('r', 'p')
                   LEFT JOIN pg_catalog.pg_attribute AS text_attribute
                     ON text_attribute.attrelid = source_class.oid
                    AND text_attribute.attname = $2
                    AND text_attribute.attnum > 0
                    AND NOT text_attribute.attisdropped
                   LEFT JOIN pg_catalog.pg_attribute AS version_attribute
                     ON version_attribute.attrelid = source_class.oid
                    AND version_attribute.attname = $3
                    AND version_attribute.attnum > 0
                    AND NOT version_attribute.attisdropped
                  WHERE collections.collection_id = $1",
                Some(1),
                &[
                    collection_id.into(),
                    text_column.into(),
                    source_version_column.into(),
                ],
            )
            .unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    "failed to validate semantic rerank source registration",
                )
            });
        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_TABLE,
                "semantic rerank source table is missing",
            );
        }
        let row = rows.first();
        (
            row.get::<bool>(1).ok().flatten().unwrap_or(false),
            row.get::<bool>(2).ok().flatten().unwrap_or(false),
            row.get::<bool>(3).ok().flatten().unwrap_or(false),
        )
    });
    if !has_select {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            "permission denied for semantic rerank source",
        );
    }
    if !text_is_valid {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
            "semantic rerank text column is missing or is not text",
        );
    }
    if !version_is_valid {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
            "semantic rerank source version column is missing or is not bigint",
        );
    }
}

/// Authorizes and persists one detached semantic-rerank request.
///
/// The returned JSON is the only text-bearing value intended to leave
/// PostgreSQL. Private request rows retain identities, hashes, fusion evidence,
/// policy, and the original query, but never duplicate authorized source text.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
#[allow(
    clippy::too_many_arguments,
    reason = "the stable SQL preparation API exposes each authority and failure-policy control explicitly"
)]
pub fn prepare_semantic_rerank(
    collection: BoundedCollectionName,
    source_name: BoundedRerankSourceName,
    query: BoundedRerankQuery,
    candidates: BoundedCandidatesJson,
    model: BoundedRerankModel,
    model_revision: i64,
    ttl_millis: default!(i64, 5000),
    failure_policy: default!(BoundedFailurePolicy, "'require_reranker'"),
    filter: default!(Option<BoundedFilterJson>, "NULL"),
    allow_partial: default!(bool, false),
) -> JsonB {
    let collection = collection.0;
    let source_name = source_name.0;
    let query = query.0;
    let model = model.0;
    let failure_policy = failure_policy.0;
    validate_source_name(&source_name);
    let query = RerankQuery::new(query).unwrap_or_else(|error| raise_query_error(error));
    let model = RerankModelName::new(model).unwrap_or_else(|error| raise_query_error(error));
    let model_revision = positive_i64(model_revision, "semantic rerank model revision");
    if !(1..=MAX_RERANK_TTL_MILLIS).contains(&ttl_millis) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "semantic rerank ttl must be 1..=60000 milliseconds",
        );
    }
    let policy = FailurePolicy::parse(&failure_policy);
    preflight_json(
        &candidates.0.0,
        MAX_RERANK_REQUEST_BYTES,
        "semantic rerank candidates",
    );
    let inputs = parse_candidate_inputs(candidates.0.0);
    let metadata_keys = candidate_metadata_keys(&inputs);
    let filter_value = filter.map(|filter| {
        context_query::validate_filter_json_value(&filter.0.0)
            .unwrap_or_else(|error| raise_query_error(error));
        filter.0.0
    });
    let parsed_filter = parse_filter(filter_value.as_ref());

    let collection_id = require_collection_owner_id(&collection);
    let source = load_locked_prepared_source(collection_id, &source_name);
    let filter_fields = load_bounded_filter_fields(
        collection_id,
        &source,
        parsed_filter.as_ref(),
        &metadata_keys,
    );
    let hydrated = hydrate_candidates(
        collection_id,
        &source,
        &inputs,
        parsed_filter.as_ref(),
        &filter_fields.fields,
        &metadata_keys,
    );
    let candidates = build_candidates(inputs, hydrated, source.content_hash_rule);
    let now_micros = postgres_now_micros();
    let ttl_micros = ttl_millis.checked_mul(1_000).unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "semantic rerank expiry overflow",
        )
    });
    let expires_at_micros = now_micros.checked_add(ttl_micros).unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "semantic rerank expiry overflow",
        )
    });

    // Build the query-owned contract before any private request row is created.
    // This is the final aggregate byte and identity check at the trust boundary.
    let provisional = RerankRequest::new(
        RerankRequestId::new(1).unwrap_or_else(|error| raise_query_error(error)),
        model.clone(),
        model_revision,
        u64::try_from(expires_at_micros).unwrap_or_else(|_| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "semantic rerank expiry is outside the envelope domain",
            )
        }),
        query.clone(),
        candidates,
    )
    .unwrap_or_else(|error| raise_query_error(error));
    validate_encoded_envelope(&provisional);
    let stored_candidates = stored_candidates_json(&provisional);
    drop(parsed_filter);
    let request_id = insert_request(
        collection_id,
        &source,
        &filter_fields.binding_sha256,
        &model,
        model_revision,
        &query,
        filter_value,
        policy,
        allow_partial,
        expires_at_micros,
        stored_candidates,
    );
    let request_id = RerankRequestId::new(u64::try_from(request_id).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "semantic rerank request identity is invalid",
        )
    }))
    .unwrap_or_else(|error| raise_query_error(error));
    let request = provisional.with_request_id(request_id);
    JsonB(envelope_json(&request))
}

/// Validates untrusted worker output and returns only currently authorized rows.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn finalize_semantic_rerank(
    request_id: i64,
    response: default!(Option<BoundedResponseJson>, "NULL"),
    failure_reason: default!(Option<BoundedFailureReason>, "NULL"),
) -> JsonB {
    let failure_reason = failure_reason.map(|reason| reason.0);
    if request_id <= 0 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "semantic rerank request identity must be positive",
        );
    }
    let attempt_digest = attempt_digest(
        response.as_ref().map(|response| &response.0),
        failure_reason.as_deref(),
    );
    let stored = load_stored_request(request_id);
    let replay = stored.status == "finalized";
    if replay {
        if stored.response_sha256.as_deref() == Some(attempt_digest.as_slice()) {
            if stored.final_result.is_none() {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    "finalized semantic rerank request has no result",
                );
            }
        } else {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                "semantic rerank request was already finalized by a different response",
            );
        }
    }

    let source = load_locked_prepared_source(stored.collection_id, &stored.source_name);
    if source.source_id != stored.source_id
        || source.registration_revision != stored.registration_revision
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "semantic rerank source registration changed before finalization",
        );
    }
    let candidates = load_stored_candidates(request_id);
    let metadata_keys = stored_candidate_metadata_keys(&candidates);
    let request = stored_request_contract(&stored, &candidates);
    let now = if replay {
        // Expiry starts at the declared instant. An identical finalized replay
        // uses the last valid microsecond so it can re-run canonical membership
        // validation while still rechecking current PostgreSQL authority.
        stored.expires_at_micros.saturating_sub(1)
    } else {
        u64::try_from(postgres_now_micros()).unwrap_or(u64::MAX)
    };
    let decision = decide_response(
        &stored,
        &request,
        response.map(|response| response.0),
        failure_reason.as_deref(),
        now,
    );
    let filter = parse_filter(stored.filter_json.as_ref().map(|json| &json.0));
    let fields = load_bounded_filter_fields(
        stored.collection_id,
        &source,
        filter.as_ref(),
        &metadata_keys,
    );
    if fields.binding_sha256 != stored.filter_binding_sha256 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "semantic rerank filter registration changed before finalization",
        );
    }
    let selected = selected_candidates(&decision, &candidates);
    let point_ids = selected
        .iter()
        .map(|selected| selected.candidate.point_id)
        .collect::<Vec<_>>();
    let mut current = hydrate_point_ids(
        stored.collection_id,
        &source,
        &point_ids,
        filter.as_ref(),
        &fields.fields,
        &metadata_keys,
    );
    let mut rows = Vec::with_capacity(selected.len());
    let mut visibility_drop = false;
    for selected in selected {
        let Some(authoritative) = current.remove(&selected.candidate.point_id) else {
            if stored.allow_partial {
                visibility_drop = true;
                continue;
            }
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
                "semantic rerank candidate visibility changed before finalization",
            );
        };
        let actual_digest = Sha256::digest(authoritative.text.as_bytes());
        if authoritative.source_version.get() != selected.candidate.source_version
            || actual_digest.as_slice() != selected.candidate.content_digest
        {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                "semantic rerank source changed before finalization",
            );
        }
        if !stored_metadata_matches(&authoritative.metadata, selected.candidate) {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                "semantic rerank source metadata changed before finalization",
            );
        }
        rows.push(json!({
            "occurrence_id": selected.candidate.occurrence_id,
            "point_id": selected.candidate.point_id,
            "score": selected.score,
            "fused_rank": selected.candidate.fused_rank,
            "fused_score": selected.candidate.fused_score,
            "source_version": selected.candidate.source_version,
            "contributions": selected.candidate.contributions.0,
            "metadata": stored_metadata_json(selected.candidate),
        }));
    }
    let partial = decision.partial || visibility_drop;
    let final_status = if decision.degraded {
        "degraded_reranker"
    } else if partial {
        "partial_reranked"
    } else {
        "reranked"
    };
    let completion = if partial { "partial" } else { "complete" };
    let result = JsonB(json!({
        "request_id": request_id,
        "status": final_status,
        "completion": completion,
        "degraded_reason": decision.degraded_reason,
        "model": stored.model.as_str(),
        "model_revision": stored.model_revision,
        "source_registration_revision": stored.registration_revision,
        "results": rows,
    }));
    if !replay {
        return complete_request(
            request_id,
            &attempt_digest,
            final_status,
            decision.degraded_reason,
            &result,
        );
    }
    result
}

/// Removes bounded expired or finalized requests owned by the session role.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn cleanup_semantic_rerank_requests(limit: default!(i32, 1000)) -> i64 {
    if !(1..=10_000).contains(&limit) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "semantic rerank cleanup limit must be 1..=10000",
        );
    }
    Spi::get_one_with_args::<i64>(
        "SELECT pgcontext._cleanup_semantic_rerank_requests($1)",
        &[limit.into()],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "semantic rerank cleanup failed",
        )
    })
    .unwrap_or(0)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FailurePolicy {
    Require,
    AllowFusedFallback,
}

impl FailurePolicy {
    fn parse(value: &str) -> Self {
        match value {
            "require_reranker" => Self::Require,
            "allow_fused_fallback" => Self::AllowFusedFallback,
            _ => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                "semantic rerank failure policy is invalid",
            ),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Require => "require_reranker",
            Self::AllowFusedFallback => "allow_fused_fallback",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidateInput {
    occurrence_id: u64,
    point_id: u64,
    fused_rank: usize,
    fused_score: f64,
    contributions: Vec<ContributionInput>,
    #[serde(default)]
    metadata: Vec<MetadataInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContributionInput {
    profile: String,
    rank: usize,
    native_score: f64,
    weight: f64,
    contribution: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MetadataInput {
    key: String,
    value: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PreparedSource {
    source_id: i64,
    registration_revision: i64,
    source_table_oid: pg_sys::Oid,
    schema_name: String,
    table_name: String,
    source_key_type_schema: String,
    source_key_type_name: String,
    text_column: String,
    version_column: String,
    content_hash_rule: ContentHashRule,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ContentHashRule {
    Sha256Utf8V1,
}

impl ContentHashRule {
    fn parse(value: &str) -> Option<Self> {
        (value == RERANK_CONTENT_HASH_RULE).then_some(Self::Sha256Utf8V1)
    }

    fn digest(self, text: &str) -> [u8; RERANK_CONTENT_DIGEST_BYTES] {
        match self {
            Self::Sha256Utf8V1 => Sha256::digest(text.as_bytes()).into(),
        }
    }
}

#[derive(Clone, Debug)]
struct HydratedSource {
    source_version: SourceVersion,
    text: String,
    metadata: BTreeMap<String, String>,
}

#[derive(Debug)]
struct LoadedFilterFields {
    fields: Vec<FilterField>,
    binding_sha256: [u8; RERANK_CONTENT_DIGEST_BYTES],
}

#[derive(Debug)]
struct StoredRequest {
    request_id: i64,
    collection_id: i64,
    source_id: i64,
    registration_revision: i64,
    filter_binding_sha256: [u8; RERANK_CONTENT_DIGEST_BYTES],
    source_name: String,
    model: RerankModelName,
    model_revision: u64,
    query: RerankQuery,
    filter_json: Option<JsonB>,
    policy: FailurePolicy,
    allow_partial: bool,
    expires_at_micros: u64,
    status: String,
    response_sha256: Option<Vec<u8>>,
    final_result: Option<JsonB>,
}

#[derive(Debug)]
struct StoredCandidate {
    occurrence_id: u64,
    point_id: u64,
    source_version: u64,
    content_digest: [u8; RERANK_CONTENT_DIGEST_BYTES],
    fused_rank: usize,
    fused_score: f64,
    contributions: JsonB,
    metadata: Vec<RerankMetadata>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponseInput {
    version: u16,
    request_id: u64,
    model: String,
    model_revision: u64,
    scores: Vec<ResponseScoreInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponseScoreInput {
    occurrence_id: u64,
    score: f64,
}

#[derive(Clone, Debug)]
struct ResponseDecision {
    scores: Option<Vec<(u64, f64)>>,
    partial: bool,
    degraded: bool,
    degraded_reason: Option<&'static str>,
}

struct SelectedCandidate<'a> {
    candidate: &'a StoredCandidate,
    score: f64,
}
