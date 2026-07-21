// HNSW metapage layout fragment included by `hnsw_am.rs`: the versioned
// metapage struct and its field/validation accessors.

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HnswMetaPage {
    magic: u32,
    version: u16,
    metric: u16,
    dimensions: u32,
    quantization_mode: u16,
    quantization_metadata_version: u16,
    graph_nodes: u64,
    entry_node_id: u64,
    scalar_min_bits: u64,
    scalar_max_bits: u64,
    scalar_levels: u32,
    pq_subvector_dimensions: u32,
    pq_codebooks_hash: u64,
    hnsw_m: u32,
    hnsw_ef_construction: u32,
    directory_epoch: u64,
    /// First main-fork block of the live base graph.
    ///
    /// A fresh build writes its base immediately after the metapage, but a
    /// compaction writes a whole new base past the end of the relation and
    /// then flips this field, so the blocks before it are superseded and
    /// must not be read back. Without this bound a compacted relation still
    /// holds the pre-compaction node pages and every node would be read
    /// twice.
    base_start_block: u64,
    /// First main-fork block of the current segmented-write delta region:
    /// every block from here to the relation's current end is a `Delta`-kind
    /// page appended since the last build or compaction. `u64::MAX` means no
    /// delta region has been established yet (an index built before this
    /// field existed, or one whose delta region a compaction has not yet
    /// re-opened).
    delta_start_block: u64,
    /// Delta records appended since `delta_start_block` was last set,
    /// including tombstones. Compared against
    /// `pgcontext.hnsw_delta_segment_limit` to decide whether an insert may
    /// still append to the delta or must fall back to the legacy inline path.
    delta_record_count: u64,
    /// Identity of the base graph generation the published node pages belong
    /// to, stamped into every node and adjacency page header as it is written.
    ///
    /// `base_start_block` bounds the base region from below but not from
    /// above, because the inline-insert path appends live base pages past the
    /// delta region and an upper bound would silently drop them. That leaves
    /// one way for a page inside the readable range to not belong to the live
    /// graph: a compaction writes a whole fresh base before it publishes, and
    /// until the flip those pages are physically present but not yet live.
    /// Readers compare this field against each page's stamp and skip the
    /// mismatches, so an unpublished — or permanently orphaned, after a crash
    /// — base is invisible rather than folded in on top of the live one.
    ///
    /// Bumped only where a new base generation is created (build and
    /// compaction), never by ordinary mutation: `directory_epoch` counts every
    /// insert and so cannot serve as this identity.
    base_generation: u64,
}

impl HnswMetaPage {
    const fn empty() -> Self {
        Self {
            magic: HNSW_META_MAGIC,
            version: HNSW_META_VERSION,
            metric: 0,
            dimensions: 0,
            quantization_mode: 0,
            quantization_metadata_version: options::HNSW_QUANTIZATION_METADATA_VERSION,
            graph_nodes: 0,
            entry_node_id: u64::MAX,
            scalar_min_bits: 0,
            scalar_max_bits: 0,
            scalar_levels: 0,
            pq_subvector_dimensions: 0,
            pq_codebooks_hash: 0,
            hnsw_m: 0,
            hnsw_ef_construction: 0,
            directory_epoch: 0,
            base_start_block: HNSW_FIRST_VECTOR_BLOCK as u64,
            delta_start_block: u64::MAX,
            delta_record_count: 0,
            base_generation: HNSW_INITIAL_PAGE_GENERATION,
        }
    }

    /// Returns the generation stamp a page being written now must carry to be
    /// read back as part of the live base graph.
    const fn page_generation(self) -> u64 {
        self.base_generation
    }

    /// Returns the generation a not-yet-published base must stamp its pages
    /// with, matching what [`Self::open_base_generation`] will publish.
    const fn next_base_generation(self) -> u64 {
        self.base_generation.saturating_add(1)
    }

    /// Starts a new base graph generation, so pages stamped for the previous
    /// one stop being read even though they remain on disk.
    ///
    /// Called under the same Generic WAL record that republishes the base, so
    /// the stamp and the region it describes become live together.
    fn open_base_generation(&mut self) {
        self.base_generation = self.base_generation.saturating_add(1);
    }

    const fn is_valid(self) -> bool {
        self.magic == HNSW_META_MAGIC
            && self.version == HNSW_META_VERSION
            && self.quantization_metadata_version <= options::HNSW_QUANTIZATION_METADATA_VERSION
    }

    fn record_build(
        &mut self,
        dimensions: Option<u32>,
        graph_nodes: u64,
        entry_point: Option<HnswNodeId>,
    ) {
        if let Some(dimensions) = dimensions {
            self.dimensions = dimensions;
        }
        self.graph_nodes = graph_nodes;
        self.entry_node_id = entry_point.map_or(u64::MAX, |node| node.get() as u64);
        self.record_directory_mutation();
    }

    fn record_index_identity(&mut self, metric: HnswScoreMetric, config: HnswConfig) {
        self.metric = metric.storage_tag();
        self.hnsw_m = usize_to_u32(config.m(), "HNSW m");
        self.hnsw_ef_construction = usize_to_u32(config.ef_construction(), "HNSW ef_construction");
    }

    fn stored_config(self, expected_metric: HnswScoreMetric, ef_search: usize) -> HnswConfig {
        if self.metric != expected_metric.storage_tag() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                format!(
                    "HNSW stored metric {} does not match opclass metric {}",
                    self.metric,
                    expected_metric.storage_tag()
                ),
            );
        }
        let m = usize::try_from(self.hnsw_m).unwrap_or_else(|_| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "HNSW stored m exceeds platform range",
            )
        });
        let ef_construction = usize::try_from(self.hnsw_ef_construction).unwrap_or_else(|_| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "HNSW stored ef_construction exceeds platform range",
            )
        });
        HnswConfig::new(m, ef_construction, ef_search).unwrap_or_else(|error| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                format!("invalid stored HNSW configuration: {error}"),
            )
        })
    }

    fn record_insert(&mut self, dimensions: u32, entry_point: Option<HnswNodeId>) -> HnswNodeId {
        let node_id = hnsw_node_id_from_graph_count(self.graph_nodes);
        if self.dimensions == 0 {
            self.dimensions = dimensions;
        } else if self.dimensions != dimensions {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!(
                    "dimension mismatch: left has {} dimensions, right has {dimensions}",
                    self.dimensions
                ),
            );
        }
        self.graph_nodes = self.graph_nodes.saturating_add(1);
        self.entry_node_id = entry_point.map_or(u64::MAX, |node| node.get() as u64);
        self.record_directory_mutation();
        node_id
    }

    fn record_directory_mutation(&mut self) {
        self.directory_epoch = self.directory_epoch.saturating_add(1);
    }

    /// Points the live base graph at a region starting at `start_block`,
    /// superseding whatever base preceded it.
    ///
    /// Compaction calls this only after every fresh base page is durable, so
    /// a crash before the flip leaves the previous base authoritative.
    fn open_base_region(&mut self, start_block: u64) {
        self.base_start_block = start_block;
    }

    /// Returns the first block a base-graph read may visit.
    ///
    /// Only the start is published, never an end. Node records appended
    /// after the build — the legacy inline-insert path — are placed by
    /// [`find_last_hnsw_page`], which puts them at the relation's end, past
    /// the delta region. Ending the base at `delta_start_block` would
    /// silently drop exactly those rows.
    ///
    /// Because there is no upper bound, this range can contain pages that are
    /// not part of the live graph — an unpublished or crash-orphaned base
    /// written by compaction. Those are excluded by generation stamp, not by
    /// block range; see [`Self::base_generation`]. A reader that filters on
    /// this bound alone is incorrect.
    const fn base_scan_start(self) -> u64 {
        // A metapage written before this field existed cannot reach here (the
        // version bump forces a rebuild), but clamp anyway: a base that began
        // at or before the metapage would read block 0 as a node page.
        if self.base_start_block < HNSW_FIRST_VECTOR_BLOCK as u64 {
            HNSW_FIRST_VECTOR_BLOCK as u64
        } else {
            self.base_start_block
        }
    }

    /// Opens (or reopens, after a compaction) the delta region starting at
    /// `block_count`: the block number of the main fork immediately after
    /// the base graph was last written. Every subsequent delta append must
    /// target `block_count` or later.
    fn open_delta_region(&mut self, block_count: u64) {
        self.delta_start_block = block_count;
        self.delta_record_count = 0;
    }

    /// Records one appended delta record (live or tombstone).
    fn record_delta_append(&mut self) {
        self.delta_record_count = self.delta_record_count.saturating_add(1);
    }

    /// Returns `true` when the delta region is open and has not yet reached
    /// `limit` records. A `limit` of `0` always returns `false` (the legacy
    /// inline-splice path), matching `pgcontext.hnsw_delta_segment_limit`'s
    /// documented `0 = disabled` convention.
    const fn delta_accepts_insert(self, limit: u64) -> bool {
        self.delta_start_block != u64::MAX && limit > 0 && self.delta_record_count < limit
    }

    fn record_quantization(&mut self, metadata: options::HnswQuantizationMetadata) {
        self.quantization_mode = metadata.mode;
        self.quantization_metadata_version = metadata.version;
        self.scalar_min_bits = metadata.scalar_min_bits;
        self.scalar_max_bits = metadata.scalar_max_bits;
        self.scalar_levels = metadata.scalar_levels;
        self.pq_subvector_dimensions = metadata.pq_subvector_dimensions;
        self.pq_codebooks_hash = metadata.pq_codebooks_hash;
    }
}

/// Returns PostgreSQL V1 function metadata for [`pgcontext_hnsw_handler`].
#[unsafe(no_mangle)]
pub extern "C-unwind" fn pg_finfo_pgcontext_hnsw_handler() -> *const pg_sys::Pg_finfo_record {
    &HNSW_HANDLER_FINFO
}

/// Returns the PostgreSQL index access-method routine for `pgcontext_hnsw`.
///
/// # Safety
///
/// PostgreSQL must call this function through its V1 function manager with a
/// valid [`pg_sys::FunctionCallInfo`] and an active memory context.
#[pg_guard]
#[allow(unused_qualifications)]
#[unsafe(no_mangle)]
// SAFETY: PostgreSQL calls this symbol through the V1 function manager after
// loading the `pg_finfo_pgcontext_hnsw_handler` metadata emitted above. The
// wrapper performs only Postgres-memory allocation and delegates all routine
// field construction to safe Rust.
pub unsafe extern "C-unwind" fn pgcontext_hnsw_handler(
    fcinfo: pg_sys::FunctionCallInfo,
) -> pg_sys::Datum {
    // SAFETY: This scope is stack-bound to the guarded handler invocation.
    let scope = unsafe { PgCallbackScope::new() };
    // SAFETY: PostgreSQL's V1 function manager supplies a live call-info
    // pointer for this guarded handler and retains ownership for the call.
    let _fcinfo = unsafe { scope.borrow(fcinfo, "FunctionCallInfo") };
    self::hnsw_handler_safe()
}

