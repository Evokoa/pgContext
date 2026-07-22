// Persisted-record and scan-key validation included by `hnsw_am.rs`. Values
// become pure graph/query types only after PostgreSQL-owned inputs are checked.

fn hnsw_graph_from_records_with_config(
    records: Vec<HnswVectorRecord>,
    metric: DistanceMetric,
    config: HnswConfig,
    entry_point: Option<HnswNodeId>,
) -> HnswGraph {
    try_hnsw_graph_from_records_with_config(records, metric, config, entry_point)
        .unwrap_or_else(|error| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                format!("failed to load persisted HNSW graph: {error}"),
            )
        })
}

fn try_hnsw_graph_from_records_with_config(
    records: Vec<HnswVectorRecord>,
    metric: DistanceMetric,
    config: HnswConfig,
    entry_point: Option<HnswNodeId>,
) -> Result<HnswGraph, HnswError> {
    let snapshots = records
        .into_iter()
        .map(hnsw_graph_snapshot_from_record)
        .collect::<Vec<_>>();

    HnswGraph::from_persisted_snapshots(metric, config, entry_point, snapshots)
}

unsafe fn hnsw_orderby_query(scan: pg_sys::IndexScanDesc) -> Option<DenseVector> {
    // SAFETY: PostgreSQL initializes `numberOfOrderBys` in live scan
    // descriptors before invoking AM scan callbacks.
    if unsafe { (*scan).numberOfOrderBys } <= 0 {
        return None;
    }
    // SAFETY: When `numberOfOrderBys` is positive, PostgreSQL provides
    // `orderByData` for the current scan descriptor.
    let orderby = unsafe { (*scan).orderByData };
    if orderby.is_null() {
        return None;
    }
    // SAFETY: `orderby` is non-null and points to the first order-by scan key.
    let is_null = unsafe { ((*orderby).sk_flags & pg_sys::SK_ISNULL.cast_signed()) != 0 };
    if is_null {
        return None;
    }
    // SAFETY: `orderby` is non-null and the argument datum is owned by
    // PostgreSQL for the duration of this scan callback.
    unsafe { hnsw_query_from_orderby_datum((*orderby).sk_argument, (*orderby).sk_subtype) }
}

unsafe fn hnsw_query_from_orderby_datum(
    datum: pg_sys::Datum,
    type_oid: pg_sys::Oid,
) -> Option<DenseVector> {
    // SAFETY: The caller passes a datum and OID from PostgreSQL scan-key
    // metadata for the active order-by operator.
    unsafe { hnsw_dense_from_datum(datum, type_oid) }
}

unsafe fn hnsw_dense_from_datum(
    datum: pg_sys::Datum,
    type_oid: pg_sys::Oid,
) -> Option<DenseVector> {
    // SAFETY: These are static, nul-terminated type names resolved in the
    // active PostgreSQL backend catalog.
    let (vector_oid, halfvec_oid, sparsevec_oid, bitvec_oid) = unsafe {
        (
            hnsw_pgcontext_type_oid(c"vector"),
            hnsw_pgcontext_type_oid(c"halfvec"),
            hnsw_pgcontext_type_oid(c"sparsevec"),
            hnsw_pgcontext_type_oid(c"bitvec"),
        )
    };

    // SAFETY: Both identifiers are static pgvector type names; the helper also
    // verifies exact extension ownership before returning either OID.
    let (pgvector_vector, pgvector_halfvec) = unsafe {
        (
            hnsw_certified_pgvector_type_oid(c"vector"),
            hnsw_certified_pgvector_type_oid(c"halfvec"),
        )
    };

    if type_oid == vector_oid || type_oid == pgvector_vector {
        // SAFETY: The scan key subtype says this argument is a SQL `vector`.
        let vector = unsafe { Vector::from_datum(datum, false) }?;
        match vector.to_dense() {
            Ok(vector) => Some(vector),
            Err(error) => raise_core_error(error),
        }
    } else if type_oid == halfvec_oid || type_oid == pgvector_halfvec {
        // SAFETY: The scan key subtype says this argument is a SQL `halfvec`.
        let halfvec = unsafe { HalfVec::from_datum(datum, false) }?;
        let halfvec = halfvec
            .to_half()
            .unwrap_or_else(|error| raise_core_error(error));
        match DenseVector::new(halfvec.as_slice().to_vec()) {
            Ok(vector) => Some(vector),
            Err(error) => raise_core_error(error),
        }
    } else if type_oid == sparsevec_oid {
        // SAFETY: The scan key subtype says this argument is a SQL `sparsevec`.
        let sparsevec = unsafe { SparseVec::from_datum(datum, false) }?;
        Some(sparsevec_to_dense(sparsevec))
    } else if type_oid == bitvec_oid {
        // SAFETY: The scan key subtype says this argument is a SQL `bitvec`.
        let bitvec = unsafe { BitVec::from_datum(datum, false) }?;
        Some(bitvec_to_dense(bitvec))
    } else {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("unsupported HNSW vector argument type oid: {type_oid}"),
        )
    }
}

unsafe fn hnsw_score_metric(index_relation: pg_sys::Relation) -> HnswScoreMetric {
    // SAFETY: The caller passes a valid index relation for the current AM
    // callback.
    let type_oid = unsafe { hnsw_index_opcintype(index_relation) };
    // SAFETY: These are static, nul-terminated type names resolved in the
    // active PostgreSQL backend catalog.
    let (vector_oid, halfvec_oid, sparsevec_oid, bitvec_oid) = unsafe {
        (
            hnsw_pgcontext_type_oid(c"vector"),
            hnsw_pgcontext_type_oid(c"halfvec"),
            hnsw_pgcontext_type_oid(c"sparsevec"),
            hnsw_pgcontext_type_oid(c"bitvec"),
        )
    };
    // SAFETY: Both static names are resolved only when owned by pgvector.
    let (pgvector_vector, pgvector_halfvec) = unsafe {
        (
            hnsw_certified_pgvector_type_oid(c"vector"),
            hnsw_certified_pgvector_type_oid(c"halfvec"),
        )
    };
    if type_oid == vector_oid {
        // SAFETY: The live single-column index relation owns initialized
        // support-proc and operator-family metadata.
        unsafe { hnsw_dense_score_metric(index_relation) }
    } else if type_oid == halfvec_oid {
        let candidates = [
            (HnswScoreMetric::L2, "halfvec_l2_distance", pg_sys::FLOAT4OID, "<->"),
            (
                HnswScoreMetric::NegativeInnerProduct,
                "halfvec_negative_inner_product",
                pg_sys::FLOAT4OID,
                "<#>",
            ),
            (
                HnswScoreMetric::Cosine,
                "halfvec_cosine_distance",
                pg_sys::FLOAT4OID,
                "<=>",
            ),
            (HnswScoreMetric::L1, "halfvec_l1_distance", pg_sys::FLOAT4OID, "<+>"),
        ];
        // SAFETY: The caller provides a live initialized index relation.
        unsafe { hnsw_score_metric_from_candidates(index_relation, &candidates, "halfvec") }
    } else if type_oid == sparsevec_oid {
        let candidates = [
            (HnswScoreMetric::L2, "sparsevec_l2_distance", pg_sys::FLOAT4OID, "<->"),
            (
                HnswScoreMetric::NegativeInnerProduct,
                "sparsevec_negative_inner_product",
                pg_sys::FLOAT4OID,
                "<#>",
            ),
            (
                HnswScoreMetric::Cosine,
                "sparsevec_cosine_distance",
                pg_sys::FLOAT4OID,
                "<=>",
            ),
            (HnswScoreMetric::L1, "sparsevec_l1_distance", pg_sys::FLOAT4OID, "<+>"),
        ];
        // SAFETY: The caller provides a live initialized index relation.
        unsafe { hnsw_score_metric_from_candidates(index_relation, &candidates, "sparsevec") }
    } else if type_oid == bitvec_oid {
        let candidates = [
            (
                HnswScoreMetric::BitHamming,
                "bitvec_hamming_distance",
                pg_sys::INT4OID,
                "<~>",
            ),
            (
                HnswScoreMetric::BitJaccard,
                "bitvec_jaccard_distance",
                pg_sys::FLOAT8OID,
                "<%>",
            ),
        ];
        // SAFETY: The caller provides a live initialized index relation.
        unsafe { hnsw_score_metric_from_candidates(index_relation, &candidates, "bitvec") }
    } else if type_oid == pgvector_vector {
        let candidates = [
            (HnswScoreMetric::L2, "_pgvector_vector_l2_support", pg_sys::FLOAT8OID, "<->"),
            (
                HnswScoreMetric::NegativeInnerProduct,
                "_pgvector_vector_ip_support",
                pg_sys::FLOAT8OID,
                "<#>",
            ),
            (
                HnswScoreMetric::Cosine,
                "_pgvector_vector_cosine_support",
                pg_sys::FLOAT8OID,
                "<=>",
            ),
            (HnswScoreMetric::L1, "_pgvector_vector_l1_support", pg_sys::FLOAT8OID, "<+>"),
        ];
        // SAFETY: The pgvector type OID was certified above and the live index
        // relation supplies the operator-family objects checked below.
        unsafe {
            hnsw_score_metric_from_bridge_candidates(index_relation, type_oid, &candidates, "vector")
        }
    } else if type_oid == pgvector_halfvec {
        let candidates = [
            (HnswScoreMetric::L2, "_pgvector_halfvec_l2_support", pg_sys::FLOAT8OID, "<->"),
            (
                HnswScoreMetric::NegativeInnerProduct,
                "_pgvector_halfvec_ip_support",
                pg_sys::FLOAT8OID,
                "<#>",
            ),
            (
                HnswScoreMetric::Cosine,
                "_pgvector_halfvec_cosine_support",
                pg_sys::FLOAT8OID,
                "<=>",
            ),
            (HnswScoreMetric::L1, "_pgvector_halfvec_l1_support", pg_sys::FLOAT8OID, "<+>"),
        ];
        // SAFETY: The pgvector type OID was certified above and the live index
        // relation supplies the operator-family objects checked below.
        unsafe {
            hnsw_score_metric_from_bridge_candidates(index_relation, type_oid, &candidates, "halfvec")
        }
    } else {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("unsupported HNSW vector input type oid: {type_oid}"),
        )
    }
}

unsafe fn hnsw_index_uses_certified_pgvector_type(index_relation: pg_sys::Relation) -> bool {
    // SAFETY: The caller provides a live single-column index relation.
    let type_oid = unsafe { hnsw_index_opcintype(index_relation) };
    // SAFETY: Static catalog identifiers are resolved and their extension
    // ownership is checked by the helpers.
    type_oid == unsafe { hnsw_certified_pgvector_type_oid(c"vector") }
        || type_oid == unsafe { hnsw_certified_pgvector_type_oid(c"halfvec") }
}

unsafe fn hnsw_orderby_contract(index_relation: pg_sys::Relation) -> HnswOrderByContract {
    // SAFETY: Both helpers inspect the same live single-column index relation;
    // score-metric validation also certifies the operator/support pairing.
    HnswOrderByContract {
        metric: unsafe { hnsw_score_metric(index_relation) },
        pgvector_binding: unsafe { hnsw_index_uses_certified_pgvector_type(index_relation) },
    }
}

unsafe fn hnsw_dense_score_metric(index_relation: pg_sys::Relation) -> HnswScoreMetric {
    let candidates = [
        (
            HnswScoreMetric::L2,
            "hnsw_l2_distance",
            pg_sys::FLOAT8OID,
            "<->",
        ),
        (
            HnswScoreMetric::NegativeInnerProduct,
            "negative_inner_product",
            pg_sys::FLOAT4OID,
            "<#>",
        ),
        (
            HnswScoreMetric::Cosine,
            "cosine_distance",
            pg_sys::FLOAT4OID,
            "<=>",
        ),
        (
            HnswScoreMetric::L1,
            "l1_distance",
            pg_sys::FLOAT4OID,
            "<+>",
        ),
    ];
    // SAFETY: The caller provides a live initialized index relation.
    unsafe { hnsw_score_metric_from_candidates(index_relation, &candidates, "vector") }
}

unsafe fn hnsw_score_metric_from_candidates(
    index_relation: pg_sys::Relation,
    candidates: &[(
        HnswScoreMetric,
        &'static str,
        pg_sys::Oid,
        &'static str,
    )],
    type_name: &str,
) -> HnswScoreMetric {
    for &(metric, support_name, return_type, operator_name) in candidates {
        // SAFETY: The caller provides a live initialized index relation.
        let type_oid = unsafe { hnsw_index_opcintype(index_relation) };
        if unsafe {
            hnsw_support_proc_matches(
                index_relation,
                support_name,
                return_type,
                type_oid,
                "pgcontext",
            )
        } {
            // SAFETY: The same relation must bind strategy 1 to the operator
            // paired with the matched metric support function.
            unsafe {
                ensure_hnsw_strategy_operator(
                    index_relation,
                    operator_name,
                    return_type,
                    "pgcontext",
                    "pgcontext",
                )
            };
            return metric;
        }
    }
    raise_sql_error(
        PgSqlErrorCode::ERRCODE_INVALID_OBJECT_DEFINITION,
        format!("HNSW {type_name} opclass must use a supported pgcontext metric function"),
    )
}

unsafe fn hnsw_score_metric_from_bridge_candidates(
    index_relation: pg_sys::Relation,
    type_oid: pg_sys::Oid,
    candidates: &[(HnswScoreMetric, &'static str, pg_sys::Oid, &'static str)],
    type_name: &str,
) -> HnswScoreMetric {
    // SAFETY: Bridge input types may only be used through an opclass that is a
    // member of the separately removable companion extension.
    unsafe { ensure_hnsw_opclass_owner(index_relation, "pgcontext_pgvector") };
    for &(metric, support_name, return_type, operator_name) in candidates {
        // SAFETY: The caller provides a live initialized index relation and a
        // type OID already certified as an extension-owned pgvector type.
        if unsafe {
            hnsw_support_proc_matches(
                index_relation,
                support_name,
                return_type,
                type_oid,
                "pgcontext_pgvector",
            )
        } {
            // SAFETY: Strategy 1 is required to be the matching operator owned
            // by pgvector, not merely a same-named public object.
            unsafe {
                ensure_hnsw_strategy_operator(
                    index_relation,
                    operator_name,
                    return_type,
                    "public",
                    "vector",
                )
            };
            return metric;
        }
    }
    raise_sql_error(
        PgSqlErrorCode::ERRCODE_INVALID_OBJECT_DEFINITION,
        format!(
            "HNSW pgvector {type_name} opclass must use a certified pgcontext_pgvector metric function"
        ),
    )
}

unsafe fn ensure_hnsw_opclass_owner(
    index_relation: pg_sys::Relation,
    expected_extension: &'static str,
) {
    if index_relation.is_null() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "HNSW index relation is not initialized",
        );
    }
    // SAFETY: The caller provides a live single-column index relation. Column
    // one is the only opclass key accepted by this AM.
    let opclass_oid = unsafe { pg_sys::get_index_column_opclass((*index_relation).rd_id, 1) };
    if opclass_oid == pg_sys::InvalidOid
        // SAFETY: The catalog lookup returned an opclass OID or InvalidOid.
        || !unsafe {
            hnsw_object_owned_by_extension(
                pg_sys::OperatorClassRelationId,
                opclass_oid,
                expected_extension,
            )
        }
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_OBJECT_DEFINITION,
            format!("HNSW opclass must be owned by extension {expected_extension}"),
        );
    }
}

unsafe fn hnsw_support_proc_matches(
    index_relation: pg_sys::Relation,
    expected_name: &'static str,
    expected_return_type: pg_sys::Oid,
    expected_argument_type: pg_sys::Oid,
    expected_extension: &'static str,
) -> bool {
    // SAFETY: PostgreSQL initializes `rd_support` for index relations. This AM
    // declares one support proc per single-column opclass.
    let support = unsafe { (*index_relation).rd_support };
    if support.is_null() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_OBJECT_DEFINITION,
            "HNSW opclass support metadata is not initialized",
        );
    }
    // SAFETY: `rd_support` has at least one entry for this single-column AM.
    let support_proc = unsafe { *support };
    // SAFETY: The support OID was read from initialized relcache metadata.
    unsafe {
        hnsw_support_proc_oid_matches(
            support_proc,
            expected_name,
            expected_return_type,
            expected_argument_type,
            expected_extension,
        )
    }
}

unsafe fn hnsw_support_proc_oid_matches(
    support_proc: pg_sys::Oid,
    expected_name: &'static str,
    expected_return_type: pg_sys::Oid,
    expected_argument_type: pg_sys::Oid,
    expected_extension: &'static str,
) -> bool {
    // SAFETY: `support_proc` is the valid support-function OID read from the
    // live relation cache entry above.
    let support_namespace = unsafe { pg_sys::get_func_namespace(support_proc) };
    // SAFETY: The same valid function OID may be resolved to its catalog name.
    let support_name = unsafe { pg_sys::get_func_name(support_proc) };
    let mut argument_types = ptr::null_mut();
    let mut argument_count = 0;
    // SAFETY: The support OID came from initialized relcache metadata. PostgreSQL
    // returns a palloc'd argument array which is released below.
    let support_return_type = unsafe {
        pg_sys::get_func_signature(support_proc, &mut argument_types, &mut argument_count)
    };
    // SAFETY: The namespace name is a static nul-terminated C string.
    let pgcontext_namespace = unsafe { pg_sys::get_namespace_oid(c"pgcontext".as_ptr(), false) };
    let valid_name = !support_name.is_null()
        // SAFETY: PostgreSQL returned a non-null nul-terminated function name
        // for the live syscache entry.
        && unsafe { CStr::from_ptr(support_name) }.to_bytes() == expected_name.as_bytes();
    let valid_arguments = argument_count == 2
        && !argument_types.is_null()
        // SAFETY: `get_func_signature` returned exactly two argument OIDs.
        && unsafe {
            *argument_types == expected_argument_type
                && *argument_types.add(1) == expected_argument_type
        };
    if !argument_types.is_null() {
        // SAFETY: PostgreSQL allocated the signature array with palloc.
        unsafe { pg_sys::pfree(argument_types.cast()) };
    }
    if !support_name.is_null() {
        // SAFETY: `get_func_name` allocated this string with palloc.
        unsafe { pg_sys::pfree(support_name.cast()) };
    }
    // SAFETY: Extension membership is checked against a catalog OID read from
    // the live relation cache.
    let valid_owner = unsafe {
        hnsw_object_owned_by_extension(
            pg_sys::ProcedureRelationId,
            support_proc,
            expected_extension,
        )
    };
    support_namespace == pgcontext_namespace
        && valid_name
        && support_return_type == expected_return_type
        && valid_arguments
        && valid_owner
}

unsafe fn ensure_hnsw_strategy_operator(
    index_relation: pg_sys::Relation,
    expected_operator_name: &'static str,
    expected_return_type: pg_sys::Oid,
    expected_namespace_name: &'static str,
    expected_extension: &'static str,
) {
    // SAFETY: PostgreSQL initializes `rd_opfamily` and `rd_opcintype` for index
    // relations. This AM registers only single-column opclasses, so the first
    // opfamily and input type are authoritative for strategy 1.
    let opfamily = unsafe { hnsw_index_opfamily(index_relation) };
    // SAFETY: The same live, single-column index relation owns the opclass input
    // type array read by this helper.
    let type_oid = unsafe { hnsw_index_opcintype(index_relation) };
    // SAFETY: The AMOPSTRATEGY syscache is keyed by opfamily, left type, right
    // type, and strategy. Strategy 1 is the only order-by strategy this AM
    // supports for its single-column opclasses.
    let tuple = unsafe {
        pg_sys::SearchSysCache4(
            pg_sys::SysCacheIdentifier::AMOPSTRATEGY.cast_signed(),
            pg_sys::ObjectIdGetDatum(opfamily),
            pg_sys::ObjectIdGetDatum(type_oid),
            pg_sys::ObjectIdGetDatum(type_oid),
            pg_sys::Int16GetDatum(1),
        )
    };
    if tuple.is_null() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_OBJECT_DEFINITION,
            "HNSW opclass strategy 1 operator metadata is not initialized",
        );
    }

    // SAFETY: `tuple` came from the syscache and is valid until ReleaseSysCache.
    let operator_oid = unsafe {
        let amop = pg_sys::GETSTRUCT(tuple) as pg_sys::Form_pg_amop;
        (*amop).amopopr
    };
    // SAFETY: `tuple` must be released after reading the syscache struct.
    unsafe { pg_sys::ReleaseSysCache(tuple) };

    // SAFETY: `operator_oid` was read from the live AMOP tuple and every
    // expected identifier is a static certified contract value.
    if !unsafe {
        hnsw_operator_oid_matches(
            operator_oid,
            type_oid,
            expected_operator_name,
            expected_return_type,
            expected_namespace_name,
            expected_extension,
        )
    } {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_OBJECT_DEFINITION,
            format!(
                "HNSW opclass must use certified {expected_namespace_name}.{expected_operator_name}"
            ),
        );
    }
}

unsafe fn hnsw_operator_oid_matches(
    operator_oid: pg_sys::Oid,
    expected_argument_type: pg_sys::Oid,
    expected_operator_name: &'static str,
    expected_return_type: pg_sys::Oid,
    expected_namespace_name: &'static str,
    expected_extension: &'static str,
) -> bool {
    // SAFETY: OPEROID is keyed by the catalog OID supplied by the caller.
    let tuple = unsafe {
        pg_sys::SearchSysCache1(
            pg_sys::SysCacheIdentifier::OPEROID.cast_signed(),
            pg_sys::ObjectIdGetDatum(operator_oid),
        )
    };
    if tuple.is_null() {
        return false;
    }
    // SAFETY: The tuple remains valid until ReleaseSysCache below.
    let (namespace, left_type, right_type, return_type) = unsafe {
        let operator = pg_sys::GETSTRUCT(tuple) as pg_sys::Form_pg_operator;
        (
            (*operator).oprnamespace,
            (*operator).oprleft,
            (*operator).oprright,
            (*operator).oprresult,
        )
    };
    // SAFETY: The syscache tuple is no longer needed.
    unsafe { pg_sys::ReleaseSysCache(tuple) };

    // SAFETY: The same valid catalog OID may be resolved to its allocated name.
    let operator_name = unsafe { pg_sys::get_opname(operator_oid) };
    let valid_name = !operator_name.is_null()
        // SAFETY: PostgreSQL returned a nul-terminated operator name.
        && unsafe { CStr::from_ptr(operator_name) }.to_bytes() == expected_operator_name.as_bytes();
    if !operator_name.is_null() {
        // SAFETY: `get_opname` allocated this string with palloc.
        unsafe { pg_sys::pfree(operator_name.cast()) };
    }
    let expected_namespace = match expected_namespace_name {
        // SAFETY: These are static nul-terminated catalog identifiers.
        "pgcontext" => unsafe { pg_sys::get_namespace_oid(c"pgcontext".as_ptr(), false) },
        // SAFETY: Same static-identifier contract as above.
        "public" => unsafe { pg_sys::get_namespace_oid(c"public".as_ptr(), false) },
        _ => return false,
    };
    namespace == expected_namespace
        && valid_name
        && left_type == expected_argument_type
        && right_type == expected_argument_type
        && return_type == expected_return_type
        // SAFETY: Extension membership accepts the live operator OID.
        && unsafe {
            hnsw_object_owned_by_extension(
                pg_sys::OperatorRelationId,
                operator_oid,
                expected_extension,
            )
        }
}

unsafe fn hnsw_validate_opclass(opclass_oid: pg_sys::Oid) -> bool {
    // SAFETY: CLAOID is keyed by the scalar OID passed to amvalidate.
    let tuple = unsafe {
        pg_sys::SearchSysCache1(
            pg_sys::SysCacheIdentifier::CLAOID.cast_signed(),
            pg_sys::ObjectIdGetDatum(opclass_oid),
        )
    };
    if tuple.is_null() {
        return false;
    }
    // SAFETY: The opclass tuple remains pinned until ReleaseSysCache below.
    let (method, family, input_type) = unsafe {
        let opclass = pg_sys::GETSTRUCT(tuple) as pg_sys::Form_pg_opclass;
        (
            (*opclass).opcmethod,
            (*opclass).opcfamily,
            (*opclass).opcintype,
        )
    };
    // SAFETY: All required scalar fields have been copied.
    unsafe { pg_sys::ReleaseSysCache(tuple) };

    // SAFETY: The AM identifier is a static catalog name. Canonical custom
    // opclasses may live in any caller-owned schema; bridge opclasses are
    // separately constrained by extension ownership below.
    let expected_method = unsafe { pg_sys::get_am_oid(c"pgcontext_hnsw".as_ptr(), true) };
    if method == pg_sys::InvalidOid || method != expected_method {
        return false;
    }

    // SAFETY: Static type lookups resolve the canonical and optionally
    // installed pgvector input types for exact OID comparison.
    let (
        canonical_vector,
        canonical_halfvec,
        canonical_sparsevec,
        canonical_bitvec,
        pgvector_vector,
        pgvector_halfvec,
        // SAFETY: Every lookup below uses a static catalog identifier; the
        // pgvector variants additionally verify exact extension ownership.
    ) = unsafe {
        (
            hnsw_pgcontext_type_oid(c"vector"),
            hnsw_pgcontext_type_oid(c"halfvec"),
            hnsw_pgcontext_type_oid(c"sparsevec"),
            hnsw_pgcontext_type_oid(c"bitvec"),
            hnsw_certified_pgvector_type_oid(c"vector"),
            hnsw_certified_pgvector_type_oid(c"halfvec"),
        )
    };

    if input_type == canonical_vector {
        let candidates = [
            ("hnsw_l2_distance", pg_sys::FLOAT8OID, "<->"),
            ("negative_inner_product", pg_sys::FLOAT4OID, "<#>"),
            ("cosine_distance", pg_sys::FLOAT4OID, "<=>"),
            ("l1_distance", pg_sys::FLOAT4OID, "<+>"),
        ];
        // SAFETY: The copied opclass OIDs and this static canonical contract
        // are valid for the duration of amvalidate.
        unsafe {
            hnsw_validate_opclass_candidates(
                opclass_oid,
                family,
                input_type,
                &candidates,
                "pgcontext",
                "pgcontext",
                "pgcontext",
            )
        }
    } else if input_type == canonical_halfvec {
        let candidates = [
            ("halfvec_l2_distance", pg_sys::FLOAT4OID, "<->"),
            ("halfvec_negative_inner_product", pg_sys::FLOAT4OID, "<#>"),
            ("halfvec_cosine_distance", pg_sys::FLOAT4OID, "<=>"),
            ("halfvec_l1_distance", pg_sys::FLOAT4OID, "<+>"),
        ];
        // SAFETY: Same copied catalog-OID and static-contract boundary above.
        unsafe {
            hnsw_validate_opclass_candidates(
                opclass_oid,
                family,
                input_type,
                &candidates,
                "pgcontext",
                "pgcontext",
                "pgcontext",
            )
        }
    } else if input_type == canonical_sparsevec {
        let candidates = [
            ("sparsevec_l2_distance", pg_sys::FLOAT4OID, "<->"),
            ("sparsevec_negative_inner_product", pg_sys::FLOAT4OID, "<#>"),
            ("sparsevec_cosine_distance", pg_sys::FLOAT4OID, "<=>"),
            ("sparsevec_l1_distance", pg_sys::FLOAT4OID, "<+>"),
        ];
        // SAFETY: Same copied catalog-OID and static-contract boundary above.
        unsafe {
            hnsw_validate_opclass_candidates(
                opclass_oid,
                family,
                input_type,
                &candidates,
                "pgcontext",
                "pgcontext",
                "pgcontext",
            )
        }
    } else if input_type == canonical_bitvec {
        let candidates = [
            ("bitvec_hamming_distance", pg_sys::INT4OID, "<~>"),
            ("bitvec_jaccard_distance", pg_sys::FLOAT8OID, "<%>"),
        ];
        // SAFETY: Same copied catalog-OID and static-contract boundary above.
        unsafe {
            hnsw_validate_opclass_candidates(
                opclass_oid,
                family,
                input_type,
                &candidates,
                "pgcontext",
                "pgcontext",
                "pgcontext",
            )
        }
    } else if input_type == pgvector_vector {
        let candidates = [
            ("_pgvector_vector_l2_support", pg_sys::FLOAT8OID, "<->"),
            ("_pgvector_vector_ip_support", pg_sys::FLOAT8OID, "<#>"),
            ("_pgvector_vector_cosine_support", pg_sys::FLOAT8OID, "<=>"),
            ("_pgvector_vector_l1_support", pg_sys::FLOAT8OID, "<+>"),
        ];
        // SAFETY: The input type was certified as pgvector-owned and the
        // remaining identifiers are the static bridge contract.
        unsafe {
            hnsw_validate_opclass_candidates(
                opclass_oid,
                family,
                input_type,
                &candidates,
                "pgcontext_pgvector",
                "public",
                "vector",
            )
        }
    } else if input_type == pgvector_halfvec {
        let candidates = [
            ("_pgvector_halfvec_l2_support", pg_sys::FLOAT8OID, "<->"),
            ("_pgvector_halfvec_ip_support", pg_sys::FLOAT8OID, "<#>"),
            ("_pgvector_halfvec_cosine_support", pg_sys::FLOAT8OID, "<=>"),
            ("_pgvector_halfvec_l1_support", pg_sys::FLOAT8OID, "<+>"),
        ];
        // SAFETY: Same certified pgvector and static bridge contract above.
        unsafe {
            hnsw_validate_opclass_candidates(
                opclass_oid,
                family,
                input_type,
                &candidates,
                "pgcontext_pgvector",
                "public",
                "vector",
            )
        }
    } else {
        false
    }
}

unsafe fn hnsw_validate_opclass_candidates(
    opclass_oid: pg_sys::Oid,
    family: pg_sys::Oid,
    input_type: pg_sys::Oid,
    candidates: &[(&'static str, pg_sys::Oid, &'static str)],
    opclass_and_support_extension: &'static str,
    operator_namespace: &'static str,
    operator_extension: &'static str,
) -> bool {
    // SAFETY: Extension ownership accepts the catalog OID supplied by
    // amvalidate. Bridge opclasses, unlike canonical custom opclasses, must be
    // members of the separately removable companion extension.
    if opclass_and_support_extension == "pgcontext_pgvector"
        && !unsafe {
            hnsw_object_owned_by_extension(
                pg_sys::OperatorClassRelationId,
                opclass_oid,
                opclass_and_support_extension,
            )
        }
    {
        return false;
    }

    // SAFETY: AMOPSTRATEGY is keyed by the copied opfamily and input type.
    let tuple = unsafe {
        pg_sys::SearchSysCache4(
            pg_sys::SysCacheIdentifier::AMOPSTRATEGY.cast_signed(),
            pg_sys::ObjectIdGetDatum(family),
            pg_sys::ObjectIdGetDatum(input_type),
            pg_sys::ObjectIdGetDatum(input_type),
            pg_sys::Int16GetDatum(1),
        )
    };
    if tuple.is_null() {
        return false;
    }
    // SAFETY: The tuple remains valid while its scalar fields are copied.
    let (operator_oid, method, sort_family, purpose) = unsafe {
        let amop = pg_sys::GETSTRUCT(tuple) as pg_sys::Form_pg_amop;
        (
            (*amop).amopopr,
            (*amop).amopmethod,
            (*amop).amopsortfamily,
            (*amop).amoppurpose,
        )
    };
    // SAFETY: All needed AMOP fields have been copied.
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    // SAFETY: Static AM lookup and catalog family/proc helpers accept the
    // copied OIDs above.
    let expected_method = unsafe { pg_sys::get_am_oid(c"pgcontext_hnsw".as_ptr(), true) };
    let support_proc = unsafe { pg_sys::get_opfamily_proc(family, input_type, input_type, 1) };
    if method != expected_method
        || purpose != pg_sys::AMOP_ORDER.cast_signed()
        || support_proc == pg_sys::InvalidOid
    {
        return false;
    }

    candidates.iter().any(|&(support_name, return_type, operator_name)| {
        // SAFETY: All OIDs come from the same opclass family and every expected
        // identifier is a static certified contract value.
        unsafe {
            hnsw_support_proc_oid_matches(
                support_proc,
                support_name,
                return_type,
                input_type,
                opclass_and_support_extension,
            ) && hnsw_operator_oid_matches(
                operator_oid,
                input_type,
                operator_name,
                return_type,
                operator_namespace,
                operator_extension,
            ) && hnsw_sort_family_matches(sort_family, return_type)
        }
    })
}

unsafe fn hnsw_sort_family_matches(sort_family: pg_sys::Oid, return_type: pg_sys::Oid) -> bool {
    let expected_name = if return_type == pg_sys::INT4OID {
        b"integer_ops".as_slice()
    } else if return_type == pg_sys::FLOAT4OID || return_type == pg_sys::FLOAT8OID {
        b"float_ops".as_slice()
    } else {
        return false;
    };
    // SAFETY: OPFAMILYOID is keyed by the sort-family OID from the AMOP row.
    let tuple = unsafe {
        pg_sys::SearchSysCache1(
            pg_sys::SysCacheIdentifier::OPFAMILYOID.cast_signed(),
            pg_sys::ObjectIdGetDatum(sort_family),
        )
    };
    if tuple.is_null() {
        return false;
    }
    // SAFETY: The NameData, method, and namespace remain valid until release.
    let matches = unsafe {
        let family = pg_sys::GETSTRUCT(tuple) as pg_sys::Form_pg_opfamily;
        let name = CStr::from_ptr((*family).opfname.data.as_ptr()).to_bytes();
        let pg_catalog = pg_sys::get_namespace_oid(c"pg_catalog".as_ptr(), false);
        let btree = pg_sys::get_am_oid(c"btree".as_ptr(), false);
        name == expected_name
            && (*family).opfnamespace == pg_catalog
            && (*family).opfmethod == btree
    };
    // SAFETY: The syscache tuple is no longer needed.
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    matches
}

unsafe fn hnsw_object_owned_by_extension(
    class_id: pg_sys::Oid,
    object_id: pg_sys::Oid,
    expected_extension: &str,
) -> bool {
    // SAFETY: PostgreSQL accepts arbitrary catalog class/object OIDs and
    // returns InvalidOid for objects that are not extension members.
    let extension_oid = unsafe { pg_sys::getExtensionOfObject(class_id, object_id) };
    if extension_oid == pg_sys::InvalidOid {
        return false;
    }
    // SAFETY: The extension OID came from pg_depend. The returned name is
    // palloc'd and released after comparison.
    let extension_name = unsafe { pg_sys::get_extension_name(extension_oid) };
    if extension_name.is_null() {
        return false;
    }
    // SAFETY: PostgreSQL returned a nul-terminated extension name.
    let matches = unsafe { CStr::from_ptr(extension_name) }.to_bytes()
        == expected_extension.as_bytes();
    // SAFETY: `get_extension_name` allocated this string with palloc.
    unsafe { pg_sys::pfree(extension_name.cast()) };
    matches
}

unsafe fn hnsw_certified_pgvector_type_oid(type_name: &'static CStr) -> pg_sys::Oid {
    // SAFETY: Both names are static nul-terminated catalog identifiers.
    let type_oid = unsafe { hnsw_named_type_oid(c"public", type_name) };
    if type_oid == pg_sys::InvalidOid {
        return pg_sys::InvalidOid;
    }
    // SAFETY: The type OID came from pg_type and is accepted by the extension
    // membership catalog lookup.
    if unsafe {
        hnsw_object_owned_by_extension(pg_sys::TypeRelationId, type_oid, "vector")
    } {
        type_oid
    } else {
        pg_sys::InvalidOid
    }
}

unsafe fn hnsw_named_type_oid(schema_name: &'static CStr, type_name: &'static CStr) -> pg_sys::Oid {
    // SAFETY: Both identifiers are static and PostgreSQL owns the namespace
    // catalog lookup for the duration of this callback.
    let namespace = unsafe { pg_sys::get_namespace_oid(schema_name.as_ptr(), true) };
    if namespace == pg_sys::InvalidOid {
        return pg_sys::InvalidOid;
    }

    let mut name = pg_sys::NameData::default();
    // SAFETY: The type name is shorter than NAMEDATALEN.
    unsafe { pg_sys::namestrcpy((&mut name) as pg_sys::Name, type_name.as_ptr()) };
    let type_oid_attribute = match pg_sys::AttrNumber::try_from(pg_sys::Anum_pg_type_oid) {
        Ok(attribute) => attribute,
        Err(_) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "pg_type OID attribute number is out of range",
        ),
    };
    // SAFETY: The cache key uses initialized catalog identifiers.
    unsafe {
        pg_sys::GetSysCacheOid(
            pg_sys::SysCacheIdentifier::TYPENAMENSP.cast_signed(),
            type_oid_attribute,
            pg_sys::NameGetDatum(&name),
            pg_sys::ObjectIdGetDatum(namespace),
            pg_sys::Datum::from(0),
            pg_sys::Datum::from(0),
        )
    }
}

unsafe fn hnsw_pgcontext_type_oid(type_name: &'static CStr) -> pg_sys::Oid {
    // SAFETY: `pgcontext` is a static null-terminated string and PostgreSQL owns
    // namespace catalog lookups for the duration of this callback.
    let pgcontext_namespace = unsafe { pg_sys::get_namespace_oid(c"pgcontext".as_ptr(), false) };
    if pgcontext_namespace == pg_sys::InvalidOid {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "pgcontext schema is not available for pgContext vector type lookup",
        );
    }

    let mut name = pg_sys::NameData::default();
    // SAFETY: `name` is a stack-allocated PostgreSQL NameData and `type_name`
    // is a static null-terminated type name shorter than NAMEDATALEN.
    unsafe { pg_sys::namestrcpy((&mut name) as pg_sys::Name, type_name.as_ptr()) };
    // SAFETY: The TYPENAMENSP syscache is keyed by (typname, typnamespace), and
    // Anum_pg_type_oid is the OID attribute to return.
    let type_oid_attribute = match pg_sys::AttrNumber::try_from(pg_sys::Anum_pg_type_oid) {
        Ok(attribute) => attribute,
        Err(_) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "pg_type OID attribute number is out of range",
        ),
    };
    // SAFETY: The cache key uses an initialized NameData and live pgcontext
    // namespace OID; PostgreSQL owns the returned catalog datum.
    let type_oid = unsafe {
        pg_sys::GetSysCacheOid(
            pg_sys::SysCacheIdentifier::TYPENAMENSP.cast_signed(),
            type_oid_attribute,
            pg_sys::NameGetDatum(&name),
            pg_sys::ObjectIdGetDatum(pgcontext_namespace),
            pg_sys::Datum::from(0),
            pg_sys::Datum::from(0),
        )
    };
    if type_oid == pg_sys::InvalidOid {
        let type_name = type_name.to_string_lossy();
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("public.{type_name} type is not available for HNSW vector decoding"),
        );
    }
    type_oid
}

unsafe fn hnsw_index_opfamily(index_relation: pg_sys::Relation) -> pg_sys::Oid {
    if index_relation.is_null() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "HNSW index relation is not initialized",
        );
    }
    // SAFETY: The caller provides a live index relation for an AM callback.
    let opfamily = unsafe { (*index_relation).rd_opfamily };
    if opfamily.is_null() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "HNSW index opfamily metadata is not initialized",
        );
    }
    // SAFETY: `rd_opfamily` points to one OID per index key. This AM registers
    // only single-column opclasses, so the first entry is authoritative.
    unsafe { *opfamily }
}

fn sparsevec_to_dense(vector: SparseVec) -> DenseVector {
    let vector = vector
        .to_sparse()
        .unwrap_or_else(|error| raise_core_error(error));
    let mut values = vec![0.0; vector.dimensions()];
    for entry in vector.entries() {
        values[entry.index() - 1] = entry.value();
    }
    DenseVector::new(values).unwrap_or_else(|error| raise_core_error(error))
}

fn bitvec_to_dense(vector: BitVec) -> DenseVector {
    let vector = vector
        .to_bit()
        .unwrap_or_else(|error| raise_core_error(error));
    let values = vector
        .as_slice()
        .iter()
        .map(|bit| if *bit { 1.0 } else { 0.0 })
        .collect::<Vec<_>>();
    DenseVector::new(values).unwrap_or_else(|error| raise_core_error(error))
}

unsafe fn hnsw_vector_from_index_values(
    index_relation: pg_sys::Relation,
    values: *mut pg_sys::Datum,
    is_null: *mut bool,
) -> Option<DenseVector> {
    // SAFETY: AM build and insert callbacks pass at least one datum and null
    // flag for the single-column opclass currently registered by this AM.
    let is_vector_null = unsafe { *is_null };
    if is_vector_null {
        return None;
    }

    // SAFETY: PostgreSQL initializes `rd_opcintype` for index relations. The
    // first input OID is the opclass input type for this single-column AM; the
    // datum itself is copied into dense HNSW storage before this callback
    // returns.
    let type_oid = unsafe { hnsw_index_opcintype(index_relation) };
    // SAFETY: The first datum matches the opclass input type read above.
    unsafe { hnsw_dense_from_datum(*values, type_oid) }
}

unsafe fn hnsw_index_opcintype(index_relation: pg_sys::Relation) -> pg_sys::Oid {
    if index_relation.is_null() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "HNSW index relation is not initialized",
        );
    }
    // SAFETY: The caller provides a live index relation for an AM callback.
    let opcintype = unsafe { (*index_relation).rd_opcintype };
    if opcintype.is_null() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "HNSW index opclass input type metadata is not initialized",
        );
    }
    // SAFETY: `rd_opcintype` points to one OID per index key. This AM registers
    // only single-column opclasses, so the first entry is authoritative.
    unsafe { *opcintype }
}

fn dimension_to_u32(dimension: usize) -> u32 {
    match u32::try_from(dimension) {
        Ok(dimension) => dimension,
        Err(_) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("vector dimensions exceed HNSW metapage storage: {dimension}"),
        ),
    }
}

fn usize_to_u32(value: usize, context: &'static str) -> u32 {
    u32::try_from(value).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("{context} exceeds HNSW metapage storage: {value}"),
        )
    })
}

fn c_int_to_usize(value: std::ffi::c_int, context: &'static str) -> usize {
    usize::try_from(value).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("{context} count must be non-negative: {value}"),
        )
    })
}

fn checked_scan_count(value: std::ffi::c_int, maximum: usize, context: &'static str) -> usize {
    let value = c_int_to_usize(value, context);
    let Some(value) = bounded_scan_count(value, maximum) else {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!("{context} count exceeds HNSW limit: {value} > {maximum}"),
        );
    };
    value
}

fn bounded_scan_count(value: usize, maximum: usize) -> Option<usize> {
    (value <= maximum).then_some(value)
}

fn checked_callback_allocation_bytes<T>(count: usize, context: &'static str) -> usize {
    callback_allocation_bytes::<T>(count).unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!("{context} allocation size overflow for {count} values"),
        )
    })
}

fn callback_allocation_bytes<T>(count: usize) -> Option<usize> {
    count.checked_mul(size_of::<T>())
}

fn checked_rescan_extent(requested: usize, capacity: usize, context: &'static str) {
    let Some(_) = bounded_rescan_count(requested, capacity) else {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("{context} count exceeds scan capacity: {requested} > {capacity}"),
        );
    };
}

fn bounded_rescan_count(requested: usize, capacity: usize) -> Option<usize> {
    (requested <= capacity).then_some(requested)
}

unsafe fn copy_rescan_keys(
    source: PgCallbackSlice<'_, pg_sys::ScanKeyData>,
    destination: *mut pg_sys::ScanKeyData,
    context: &'static str,
) {
    if source.len() > 1 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!("{context} source exceeds the one-key HNSW limit"),
        );
    }
    let source_value = {
        let source_slice = source.as_slice();
        source_slice.first().copied()
    };
    let Some(source_value) = source_value else {
        return;
    };
    if destination.is_null() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("HNSW scan descriptor has null {context} destination"),
        );
    }
    // SAFETY: The caller proves one writable destination element. The source
    // reference ended before this write and the copied value is stack-owned,
    // so even an identical source/destination address cannot alias a live ref.
    unsafe { ptr::write(destination, source_value) };
}

#[allow(
    clippy::cast_precision_loss,
    reason = "PostgreSQL IndexBuildResult stores index tuple counts as double estimates"
)]
fn u64_to_pg_estimate_f64(value: u64) -> f64 {
    value as f64
}

fn hnsw_node_id_from_graph_count(graph_nodes: u64) -> HnswNodeId {
    match checked_hnsw_node_id_from_graph_count(graph_nodes) {
        Ok(node_id) => node_id,
        Err(graph_nodes) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!("HNSW graph node count exceeds page-record storage: {graph_nodes}"),
        ),
    }
}

fn checked_hnsw_node_id_from_graph_count(graph_nodes: u64) -> Result<HnswNodeId, u64> {
    u32::try_from(graph_nodes)
        .map(|node_id| HnswNodeId::new(node_id as usize))
        .map_err(|_| graph_nodes)
}
