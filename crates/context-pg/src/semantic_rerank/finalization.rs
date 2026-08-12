fn attempt_digest(response: Option<&JsonB>, failure_reason: Option<&str>) -> Vec<u8> {
    let mut hasher = Sha256::new();
    match (response, failure_reason) {
        (Some(response), None) => {
            preflight_json(
                &response.0,
                MAX_RERANK_REQUEST_BYTES,
                "semantic rerank response",
            );
            hasher.update(b"response\0");
            let bytes = serde_json::to_vec(&response.0).unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                    "semantic rerank response is malformed",
                )
            });
            hasher.update(bytes);
        }
        (None, Some(reason)) => {
            validate_failure_reason(reason);
            hasher.update(b"failure\0");
            hasher.update(reason.as_bytes());
        }
        _ => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "provide exactly one semantic rerank response or failure reason",
        ),
    }
    hasher.finalize().to_vec()
}

fn validate_failure_reason(reason: &str) {
    match reason {
        "unavailable" | "timeout" | "crash" | "partial_output" | "expired" => {}
        "cancelled" => raise_sql_error(
            PgSqlErrorCode::ERRCODE_QUERY_CANCELED,
            "semantic rerank was cancelled",
        ),
        _ => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "semantic rerank failure reason is invalid",
        ),
    }
}

fn load_stored_request(request_id: i64) -> StoredRequest {
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT requests.request_id,
                        requests.collection_id,
                        requests.rerank_source_id,
                        requests.source_registration_revision,
                        requests.filter_binding_sha256,
                        sources.source_name,
                        requests.model_name,
                        requests.model_revision,
                        requests.query_text,
                        CASE WHEN requests.filter_json = 'null'::jsonb
                             THEN NULL ELSE requests.filter_json END,
                        requests.failure_policy,
                        requests.allow_partial,
                        requests.expires_at_micros,
                        requests.status,
                        requests.response_sha256,
                        requests.final_result
                   FROM pgcontext._visible_semantic_rerank_requests AS requests
                   JOIN pgcontext._visible_semantic_rerank_sources AS sources
                     ON sources.rerank_source_id = requests.rerank_source_id
                  WHERE requests.request_id = $1",
                Some(1),
                &[request_id.into()],
            )
            .unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    "failed to load semantic rerank request",
                )
            });
        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                "semantic rerank request does not exist",
            );
        }
        let row = rows.first();
        let stored_filter_digest = required_first_column::<Vec<u8>>(&row, 5, "filter binding digest");
        let filter_binding_sha256 = stored_filter_digest.try_into().unwrap_or_else(|_| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "semantic rerank stored filter binding digest is invalid",
            )
        });
        let model = RerankModelName::new(required_first_column::<String>(&row, 7, "model"))
            .unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    "semantic rerank stored model is invalid",
                )
            });
        let query = RerankQuery::new(required_first_column::<String>(&row, 9, "query"))
            .unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    "semantic rerank stored query is invalid",
                )
            });
        StoredRequest {
            request_id: required_first_column(&row, 1, "request identity"),
            collection_id: required_first_column(&row, 2, "collection identity"),
            source_id: required_first_column(&row, 3, "source identity"),
            registration_revision: required_first_column(&row, 4, "registration revision"),
            filter_binding_sha256,
            source_name: required_first_column(&row, 6, "source name"),
            model,
            model_revision: positive_stored(
                required_first_column(&row, 8, "model revision"),
                "model revision",
            ),
            query,
            filter_json: optional_first_column(&row, 10, "filter"),
            policy: FailurePolicy::parse(&required_first_column::<String>(&row, 11, "policy")),
            allow_partial: required_first_column(&row, 12, "allow partial"),
            expires_at_micros: positive_stored(required_first_column(&row, 13, "expiry"), "expiry"),
            status: required_first_column(&row, 14, "request status"),
            response_sha256: optional_first_column(&row, 15, "response digest"),
            final_result: optional_first_column(&row, 16, "final result"),
        }
    })
}

fn positive_stored(value: i64, label: &'static str) -> u64 {
    u64::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                format!("semantic rerank stored {label} is invalid"),
            )
        })
}

fn load_stored_candidates(request_id: i64) -> Vec<StoredCandidate> {
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT occurrence_id, point_id, source_version, content_digest,
                        fused_rank, fused_score, contributions, metadata
                   FROM pgcontext._visible_semantic_rerank_candidates
                  WHERE request_id = $1
                  ORDER BY occurrence_id",
                Some(i64::try_from(MAX_RERANK_CANDIDATES + 1).unwrap_or(i64::MAX)),
                &[request_id.into()],
            )
            .unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    "failed to load semantic rerank candidates",
                )
            });
        if rows.is_empty() || rows.len() > MAX_RERANK_CANDIDATES {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "semantic rerank stored candidate count is invalid",
            );
        }
        rows.into_iter()
            .map(|row| {
                let digest = required_column::<Vec<u8>>(&row, 4, "content digest");
                let content_digest = digest.try_into().unwrap_or_else(|_| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                        "semantic rerank stored content digest is invalid",
                    )
                });
                let fused_rank = usize::try_from(required_column::<i32>(&row, 5, "fused rank"))
                    .ok()
                    .filter(|rank| *rank > 0)
                    .unwrap_or_else(|| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                            "semantic rerank stored fused rank is invalid",
                        )
                    });
                StoredCandidate {
                    occurrence_id: positive_stored(
                        required_column(&row, 1, "occurrence identity"),
                        "occurrence identity",
                    ),
                    point_id: positive_stored(
                        required_column(&row, 2, "point identity"),
                        "point identity",
                    ),
                    source_version: positive_stored(
                        required_column(&row, 3, "source version"),
                        "source version",
                    ),
                    content_digest,
                    fused_rank,
                    fused_score: required_column(&row, 6, "fused score"),
                    contributions: required_column(&row, 7, "contributions"),
                    metadata: parse_stored_metadata(required_column(&row, 8, "metadata")),
                }
            })
            .collect()
    })
}

fn parse_stored_metadata(metadata: JsonB) -> Vec<RerankMetadata> {
    preflight_json(
        &metadata.0,
        MAX_RERANK_METADATA_ENTRIES * (2 * MAX_RERANK_METADATA_BYTES + size_of::<Value>()),
        "stored semantic rerank metadata",
    );
    let inputs = serde_json::from_value::<Vec<MetadataInput>>(metadata.0).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "semantic rerank stored metadata is invalid",
        )
    });
    if inputs.len() > MAX_RERANK_METADATA_ENTRIES {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "semantic rerank stored metadata count is invalid",
        );
    }
    let mut keys = BTreeSet::new();
    inputs
        .into_iter()
        .map(|metadata| {
            if !keys.insert(metadata.key.clone()) {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    "semantic rerank stored metadata repeats a key",
                );
            }
            RerankMetadata::new(metadata.key, metadata.value).unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    "semantic rerank stored metadata contract is invalid",
                )
            })
        })
        .collect()
}

fn stored_candidate_metadata_keys(candidates: &[StoredCandidate]) -> BTreeSet<String> {
    let keys = candidates
        .iter()
        .flat_map(|candidate| candidate.metadata.iter())
        .map(|metadata| metadata.key().to_owned())
        .collect::<BTreeSet<_>>();
    if keys.len() > MAX_RERANK_METADATA_ENTRIES {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "semantic rerank stored metadata allow-list is invalid",
        );
    }
    keys
}

fn stored_metadata_matches(
    authoritative: &BTreeMap<String, String>,
    candidate: &StoredCandidate,
) -> bool {
    candidate.metadata.iter().all(|metadata| {
        authoritative.get(metadata.key()).map(String::as_str) == Some(metadata.value())
    })
}

fn stored_metadata_json(candidate: &StoredCandidate) -> Value {
    Value::Array(
        candidate
            .metadata
            .iter()
            .map(|metadata| json!({"key": metadata.key(), "value": metadata.value()}))
            .collect(),
    )
}

fn stored_request_contract(
    stored: &StoredRequest,
    candidates: &[StoredCandidate],
) -> RerankRequest {
    let request_id = RerankRequestId::new(u64::try_from(stored.request_id).unwrap_or(0))
        .unwrap_or_else(|_| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "semantic rerank stored request identity is invalid",
            )
        });
    let candidates = candidates
        .iter()
        .map(stored_candidate_contract)
        .collect::<Vec<_>>();
    RerankRequest::new(
        request_id,
        stored.model.clone(),
        stored.model_revision,
        stored.expires_at_micros,
        stored.query.clone(),
        candidates,
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "semantic rerank stored request contract is invalid",
        )
    })
}

fn stored_candidate_contract(candidate: &StoredCandidate) -> RerankCandidate {
    let contributions =
        serde_json::from_value::<Vec<ContributionInput>>(candidate.contributions.0.clone())
            .unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    "semantic rerank stored contributions are invalid",
                )
            })
            .into_iter()
            .map(|contribution| {
                RerankContribution::new(
                    contribution.profile,
                    contribution.rank,
                    contribution.native_score,
                    contribution.weight,
                    contribution.contribution,
                )
                .unwrap_or_else(|_| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                        "semantic rerank stored contribution contract is invalid",
                    )
                })
            })
            .collect();
    let metadata = candidate.metadata.clone();
    RerankCandidate::new(
        OccurrenceId::new(candidate.occurrence_id).unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "semantic rerank stored occurrence identity is invalid",
            )
        }),
        PointId::new(candidate.point_id),
        SourceVersion::new(candidate.source_version).unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "semantic rerank stored source version is invalid",
            )
        }),
        RerankContentDigest::new(candidate.content_digest),
        String::new(),
        candidate.fused_rank,
        candidate.fused_score,
        contributions,
        metadata,
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "semantic rerank stored candidate contract is invalid",
        )
    })
}

fn decide_response(
    stored: &StoredRequest,
    request: &RerankRequest,
    response: Option<JsonB>,
    failure_reason: Option<&str>,
    now_micros: u64,
) -> ResponseDecision {
    let Some(response) = response else {
        let reason = failure_reason.unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                "semantic rerank failure reason is missing",
            )
        });
        return fallback_decision(stored.policy, reason);
    };
    if failure_reason.is_some() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "provide exactly one semantic rerank response or failure reason",
        );
    }
    let input = serde_json::from_value::<ResponseInput>(response.0).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "semantic rerank response is malformed",
        )
    });
    if input.scores.len() > MAX_RERANK_CANDIDATES {
        reject_provider_response(RerankRejection::TooManyScores);
    }
    let model = RerankModelName::new(input.model)
        .unwrap_or_else(|_| reject_provider_response(RerankRejection::ModelMismatch));
    let request_id = RerankRequestId::new(input.request_id)
        .unwrap_or_else(|_| reject_provider_response(RerankRejection::RequestMismatch));
    let scores = input
        .scores
        .into_iter()
        .map(|score| {
            let occurrence = OccurrenceId::new(score.occurrence_id)
                .unwrap_or_else(|| reject_provider_response(RerankRejection::UnknownOccurrence));
            RerankScore::new(occurrence, score.score).unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                    "semantic rerank response contains an invalid score",
                )
            })
        })
        .collect();
    let response = RerankResponse::new(
        input.version,
        request_id,
        model,
        input.model_revision,
        scores,
    );
    let policy = if stored.allow_partial {
        RerankResponsePolicy::AllowPartial
    } else {
        RerankResponsePolicy::RequireComplete
    };
    let validated =
        match validate_rerank_response_with_policy(request, &response, now_micros, policy) {
            Ok(validated) => validated,
            Err(RerankRejection::Expired) if stored.policy == FailurePolicy::AllowFusedFallback => {
                return fallback_decision(stored.policy, "expired");
            }
            Err(RerankRejection::Incomplete)
                if stored.policy == FailurePolicy::AllowFusedFallback =>
            {
                return fallback_decision(stored.policy, "partial_output");
            }
            Err(rejection) => reject_provider_response(rejection),
        };
    let partial = validated.completion() == RerankResponseCompletion::Partial;
    let scores = validated
        .into_ordered_scores()
        .into_iter()
        .map(|score| (score.occurrence_id().get(), score.score()))
        .collect();
    ResponseDecision {
        scores: Some(scores),
        partial,
        degraded: false,
        degraded_reason: None,
    }
}

fn fallback_decision(policy: FailurePolicy, reason: &str) -> ResponseDecision {
    if policy == FailurePolicy::Require {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "semantic reranker did not produce a usable response",
        );
    }
    let reason = match reason {
        "unavailable" => "unavailable",
        "timeout" => "timeout",
        "crash" => "crash",
        "partial_output" => "partial_output",
        "expired" => "expired",
        _ => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "semantic rerank failure reason is invalid",
        ),
    };
    ResponseDecision {
        scores: None,
        partial: false,
        degraded: true,
        degraded_reason: Some(reason),
    }
}

fn reject_provider_response(rejection: RerankRejection) -> ! {
    raise_sql_error(
        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
        format!(
            "semantic rerank response failed validation: {}",
            rejection.stable_name()
        ),
    )
}

fn selected_candidates<'a>(
    decision: &ResponseDecision,
    candidates: &'a [StoredCandidate],
) -> Vec<SelectedCandidate<'a>> {
    let mut selected = if let Some(scores) = decision.scores.as_ref() {
        let candidates_by_occurrence = candidates
            .iter()
            .map(|candidate| (candidate.occurrence_id, candidate))
            .collect::<BTreeMap<_, _>>();
        scores
            .iter()
            .map(|(occurrence_id, score)| SelectedCandidate {
                candidate: candidates_by_occurrence
                    .get(occurrence_id)
                    .copied()
                    .unwrap_or_else(|| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                            "validated semantic rerank occurrence is missing",
                        )
                    }),
                score: *score,
            })
            .collect::<Vec<_>>()
    } else {
        candidates
            .iter()
            .map(|candidate| SelectedCandidate {
                candidate,
                score: candidate.fused_score,
            })
            .collect::<Vec<_>>()
    };
    if decision.scores.is_none() {
        selected.sort_by_key(|candidate| {
            (
                candidate.candidate.fused_rank,
                candidate.candidate.occurrence_id,
            )
        });
    }
    selected
}

fn complete_request(
    request_id: i64,
    attempt_digest: &[u8],
    final_status: &str,
    degraded_reason: Option<&str>,
    result: &JsonB,
) -> JsonB {
    let completed = Spi::get_one_with_args::<bool>(
        "SELECT pgcontext._finalize_semantic_rerank_request($1, $2, $3, $4, $5)",
        &[
            request_id.into(),
            attempt_digest.into(),
            final_status.into(),
            degraded_reason.into(),
            JsonB(result.0.clone()).into(),
        ],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "failed to finalize semantic rerank request",
        )
    })
    .unwrap_or(false);
    if completed {
        return JsonB(result.0.clone());
    }
    let raced = load_stored_request(request_id);
    if raced.status == "finalized"
        && raced.response_sha256.as_deref() == Some(attempt_digest)
        && raced.final_result.as_ref().map(|stored| &stored.0) == Some(&result.0)
    {
        let Some(final_result) = raced.final_result else {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "finalized semantic rerank request has no result",
            );
        };
        return final_result;
    }
    raise_sql_error(
        PgSqlErrorCode::ERRCODE_T_R_SERIALIZATION_FAILURE,
        "semantic rerank request changed during finalization",
    )
}

fn optional_first_column<T: FromDatum + IntoDatum>(
    row: &spi::SpiTupleTable<'_>,
    ordinal: usize,
    label: &'static str,
) -> Option<T> {
    row.get::<T>(ordinal).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to read semantic rerank {label}"),
        )
    })
}

fn required_first_column<T: FromDatum + IntoDatum>(
    row: &spi::SpiTupleTable<'_>,
    ordinal: usize,
    label: &'static str,
) -> T {
    row.get::<T>(ordinal)
        .unwrap_or_else(|_| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to read semantic rerank {label}"),
            )
        })
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                format!("semantic rerank {label} is null"),
            )
        })
}

fn required_column<T: FromDatum + IntoDatum>(
    row: &spi::SpiHeapTupleData<'_>,
    ordinal: usize,
    label: &'static str,
) -> T {
    row.get::<T>(ordinal)
        .unwrap_or_else(|_| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to read semantic rerank {label}"),
            )
        })
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                format!("semantic rerank {label} is null"),
            )
        })
}

fn optional_column<T: FromDatum + IntoDatum>(
    row: &spi::SpiHeapTupleData<'_>,
    ordinal: usize,
    label: &'static str,
) -> Option<T> {
    row.get::<T>(ordinal).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to read semantic rerank {label}"),
        )
    })
}
