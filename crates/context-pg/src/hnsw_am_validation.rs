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

    if type_oid == vector_oid {
        // SAFETY: The scan key subtype says this argument is a SQL `vector`.
        let vector = unsafe { Vector::from_datum(datum, false) }?;
        match vector.to_dense() {
            Ok(vector) => Some(vector),
            Err(error) => raise_core_error(error),
        }
    } else if type_oid == halfvec_oid {
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
    } else {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("unsupported HNSW vector input type oid: {type_oid}"),
        )
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
        if unsafe { hnsw_support_proc_matches(index_relation, support_name, return_type) } {
            // SAFETY: The same relation must bind strategy 1 to the operator
            // paired with the matched metric support function.
            unsafe { ensure_hnsw_strategy_operator(index_relation, operator_name) };
            return metric;
        }
    }
    raise_sql_error(
        PgSqlErrorCode::ERRCODE_INVALID_OBJECT_DEFINITION,
        format!("HNSW {type_name} opclass must use a supported pgcontext metric function"),
    )
}

unsafe fn hnsw_support_proc_matches(
    index_relation: pg_sys::Relation,
    expected_name: &'static str,
    expected_return_type: pg_sys::Oid,
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
    // SAFETY: `support_proc` is the valid support-function OID read from the
    // live relation cache entry above.
    let support_namespace = unsafe { pg_sys::get_func_namespace(support_proc) };
    // SAFETY: The same valid function OID may be resolved to its catalog name.
    let support_name = unsafe { pg_sys::get_func_name(support_proc) };
    // SAFETY: The same valid function OID may be resolved to its return type.
    let support_return_type = unsafe { pg_sys::get_func_rettype(support_proc) };
    // SAFETY: The namespace name is a static nul-terminated C string.
    let pgcontext_namespace = unsafe { pg_sys::get_namespace_oid(c"pgcontext".as_ptr(), false) };
    let valid_name = !support_name.is_null()
        // SAFETY: PostgreSQL returned a non-null nul-terminated function name
        // for the live syscache entry.
        && unsafe { CStr::from_ptr(support_name) }.to_bytes() == expected_name.as_bytes();
    support_namespace == pgcontext_namespace
        && valid_name
        && support_return_type == expected_return_type
}

unsafe fn ensure_hnsw_strategy_operator(
    index_relation: pg_sys::Relation,
    expected_operator_name: &'static str,
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

    // SAFETY: `operator_oid` was read from the live amop syscache tuple.
    let operator_namespace = unsafe { hnsw_operator_namespace(operator_oid) };
    // SAFETY: The same catalog operator OID may be resolved to its name.
    let operator_name = unsafe { pg_sys::get_opname(operator_oid) };
    // SAFETY: The namespace name is a static nul-terminated C string.
    let pgcontext_namespace = unsafe { pg_sys::get_namespace_oid(c"pgcontext".as_ptr(), false) };
    let valid_name = !operator_name.is_null()
        // SAFETY: PostgreSQL returned a non-null nul-terminated operator name
        // for the live syscache entry.
        && unsafe { CStr::from_ptr(operator_name) }.to_bytes() == expected_operator_name.as_bytes();
    if operator_namespace != pgcontext_namespace || !valid_name {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_OBJECT_DEFINITION,
            format!("HNSW opclass must use pgcontext.{expected_operator_name}"),
        );
    }
}

unsafe fn hnsw_operator_namespace(operator_oid: pg_sys::Oid) -> pg_sys::Oid {
    // SAFETY: OPEROID syscache is keyed by operator OID and returns a
    // pg_operator tuple valid until ReleaseSysCache.
    let tuple = unsafe {
        pg_sys::SearchSysCache1(
            pg_sys::SysCacheIdentifier::OPEROID.cast_signed(),
            pg_sys::ObjectIdGetDatum(operator_oid),
        )
    };
    if tuple.is_null() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_OBJECT_DEFINITION,
            "HNSW opclass strategy 1 operator catalog row is not available",
        );
    }
    // SAFETY: `tuple` came from the syscache and is valid until ReleaseSysCache.
    let operator_namespace = unsafe {
        let operator = pg_sys::GETSTRUCT(tuple) as pg_sys::Form_pg_operator;
        (*operator).oprnamespace
    };
    // SAFETY: `tuple` must be released after reading the syscache struct.
    unsafe { pg_sys::ReleaseSysCache(tuple) };
    operator_namespace
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
