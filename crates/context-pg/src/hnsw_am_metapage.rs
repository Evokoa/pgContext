// HNSW metapage layout fragment included by `hnsw_am.rs`: the versioned
// metapage struct and its field/validation accessors.

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HnswSegmentMeta {
    segment_id: u64,
    generation: u64,
    start_block: u64,
    end_block: u64,
    graph_nodes: u64,
    entry_node_id: u64,
    mutation_generation: u64,
    mutation_start_block: u64,
    mutation_end_block: u64,
    mutation_record_count: u64,
}

impl HnswSegmentMeta {
    const EMPTY: Self = Self {
        segment_id: 0,
        generation: 0,
        start_block: 0,
        end_block: 0,
        graph_nodes: 0,
        entry_node_id: u64::MAX,
        mutation_generation: u64::MAX,
        mutation_start_block: u64::MAX,
        mutation_end_block: u64::MAX,
        mutation_record_count: 0,
    };

    const fn is_empty(self) -> bool {
        self.segment_id == 0
    }

}

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
    codec_config_revision: u64,
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
    /// Exclusive end block of the active delta extent.
    delta_end_block: u64,
    /// Generation stamp accepted for active delta pages.
    delta_generation: u64,
    /// Delta records appended since `delta_start_block` was last set,
    /// including tombstones. Compared against
    /// `pgcontext.hnsw_delta_segment_limit` to decide whether an insert may
    /// still append to the delta or must rotate a bounded segment.
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
    /// Number of initialized entries in `segments`.
    segment_count: u16,
    segment_reserved16: u16,
    segment_reserved32: u32,
    /// Next stable segment identity. Zero is permanently reserved.
    next_segment_id: u64,
    /// Immutable graph extents published atomically with this metapage.
    segments: [HnswSegmentMeta; HNSW_MAX_SEGMENTS],
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "validated SQL f64 reloptions are explicitly narrowed to the f32 vector domain"
)]
fn stored_codec_bound(bits: u64, name: &'static str) -> f32 {
    let value = f64::from_bits(bits);
    if !value.is_finite() || value < f64::from(f32::MIN) || value > f64::from(f32::MAX) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            format!("stored HNSW scalar {name} bound is outside the f32 vector domain"),
        );
    }
    value as f32
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
            codec_config_revision: 0,
            hnsw_m: 0,
            hnsw_ef_construction: 0,
            directory_epoch: 0,
            base_start_block: HNSW_FIRST_VECTOR_BLOCK as u64,
            delta_start_block: u64::MAX,
            delta_end_block: u64::MAX,
            delta_generation: HNSW_INITIAL_PAGE_GENERATION,
            delta_record_count: 0,
            base_generation: HNSW_INITIAL_PAGE_GENERATION,
            segment_count: 0,
            segment_reserved16: 0,
            segment_reserved32: 0,
            next_segment_id: 1,
            segments: [HnswSegmentMeta::EMPTY; HNSW_MAX_SEGMENTS],
        }
    }

    fn codec_spec(self) -> Option<CodecSpec> {
        match self.quantization_mode {
            options::HNSW_QUANTIZATION_NONE_U16 => None,
            options::HNSW_QUANTIZATION_BINARY_U16 => Some(CodecSpec::binary()),
            options::HNSW_QUANTIZATION_SCALAR_U16 | options::HNSW_QUANTIZATION_SQ8_U16 => {
                let bounds = ScalarBounds::new(
                    stored_codec_bound(self.scalar_min_bits, "minimum"),
                    stored_codec_bound(self.scalar_max_bits, "maximum"),
                )
                .unwrap_or_else(|error| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                        format!("invalid stored HNSW scalar codec bounds: {error}"),
                    )
                });
                let levels = u16::try_from(self.scalar_levels).unwrap_or_else(|_| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                        "stored HNSW scalar levels exceed codec range",
                    )
                });
                Some(CodecSpec::scalar(levels, Some(bounds)).unwrap_or_else(|error| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                        format!("invalid stored HNSW scalar codec: {error}"),
                    )
                }))
            }
            options::HNSW_QUANTIZATION_PQ_U16 => {
                let subvector_dimensions = usize::try_from(self.pq_subvector_dimensions)
                    .unwrap_or_else(|_| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                            "stored HNSW product subvector width exceeds platform range",
                        )
                    });
                Some(
                    CodecSpec::product(subvector_dimensions, 256, 8).unwrap_or_else(|error| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                            format!("invalid stored HNSW product codec: {error}"),
                        )
                    }),
                )
            }
            _ => raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "stored HNSW quantization mode is unsupported",
            ),
        }
    }

    fn accepts_codec_codebook(self, codebook: Option<&QuantizedCodebook>) -> bool {
        let Some(spec) = self.codec_spec() else {
            return codebook.is_none();
        };
        let Some(codebook) = codebook else {
            return false;
        };
        if codebook.dimensions() != self.dimensions as usize {
            return false;
        }
        match (spec.kind(), codebook) {
            (CodecKind::Binary, QuantizedCodebook::Binary { .. }) => true,
            (
                CodecKind::Scalar,
                QuantizedCodebook::Scalar {
                    minimum,
                    maximum,
                    levels,
                    ..
                },
            ) => spec.scalar_parameters().is_some_and(|(expected_levels, bounds)| {
                *levels == expected_levels
                    && bounds.is_none_or(|bounds| {
                        minimum.to_bits() == bounds.minimum().to_bits()
                            && maximum.to_bits() == bounds.maximum().to_bits()
                    })
            }),
            (
                CodecKind::Product,
                QuantizedCodebook::Product {
                    subvector_dimensions,
                    codebooks,
                    ..
                },
            ) => spec.product_parameters().is_some_and(
                |(expected_subvector_dimensions, maximum_centroids, _)| {
                    expected_subvector_dimensions
                        .is_none_or(|expected| expected == *subvector_dimensions)
                        && codebooks.iter().all(|centroids| {
                            !centroids.is_empty() && centroids.len() <= maximum_centroids
                        })
                },
            ),
            (CodecKind::Plain, _) | (_, _) => false,
        }
    }

    const fn codec_name(self) -> &'static str {
        match self.quantization_mode {
            options::HNSW_QUANTIZATION_NONE_U16 => "none",
            options::HNSW_QUANTIZATION_BINARY_U16 => "binary",
            options::HNSW_QUANTIZATION_SCALAR_U16 => "scalar",
            options::HNSW_QUANTIZATION_SQ8_U16 => "sq8",
            options::HNSW_QUANTIZATION_PQ_U16 => "pq",
            _ => "unsupported",
        }
    }

    fn codec_code_width(self) -> Option<usize> {
        let dimensions = usize::try_from(self.dimensions).ok()?;
        match self.quantization_mode {
            options::HNSW_QUANTIZATION_NONE_U16 => None,
            options::HNSW_QUANTIZATION_BINARY_U16 => Some(dimensions.div_ceil(8)),
            options::HNSW_QUANTIZATION_SCALAR_U16 | options::HNSW_QUANTIZATION_SQ8_U16 => {
                Some(dimensions)
            }
            options::HNSW_QUANTIZATION_PQ_U16 => {
                let subvector = usize::try_from(self.pq_subvector_dimensions).ok()?;
                (subvector != 0 && dimensions.is_multiple_of(subvector))
                    .then_some(dimensions / subvector)
            }
            _ => None,
        }
    }

    fn segments(&self) -> &[HnswSegmentMeta] {
        &self.segments[..usize::from(self.segment_count)]
    }

    fn primary_segment(self) -> Option<HnswSegmentMeta> {
        self.segments().first().copied()
    }

    fn next_segment_generation(self) -> u64 {
        self.segments()
            .iter()
            .map(|segment| segment.generation)
            .max()
            .unwrap_or(HNSW_INITIAL_PAGE_GENERATION)
            .saturating_add(1)
    }

    fn publish_single_segment(
        &mut self,
        start_block: u64,
        end_block: u64,
        graph_nodes: u64,
        entry_point: Option<HnswNodeId>,
    ) {
        if graph_nodes == 0 {
            self.segment_count = 0;
            self.segments = [HnswSegmentMeta::EMPTY; HNSW_MAX_SEGMENTS];
            self.next_segment_id = self.next_segment_id.saturating_add(1).max(1);
            return;
        }
        if start_block >= end_block {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "cannot publish a non-empty HNSW segment with an empty block extent",
            );
        }
        let segment_id = self.next_segment_id.max(1);
        let generation = self.base_generation;
        self.segments = [HnswSegmentMeta::EMPTY; HNSW_MAX_SEGMENTS];
        self.segments[0] = HnswSegmentMeta {
            segment_id,
            generation,
            start_block,
            end_block,
            graph_nodes,
            entry_node_id: entry_point.map_or(u64::MAX, |node| node.get() as u64),
            mutation_generation: u64::MAX,
            mutation_start_block: u64::MAX,
            mutation_end_block: u64::MAX,
            mutation_record_count: 0,
        };
        self.segment_count = 1;
        self.next_segment_id = segment_id.saturating_add(1);
        self.base_start_block = start_block;
        self.graph_nodes = graph_nodes;
        self.entry_node_id = entry_point.map_or(u64::MAX, |node| node.get() as u64);
    }

    fn publish_additional_segment(
        &mut self,
        generation: u64,
        start_block: u64,
        end_block: u64,
        graph_nodes: u64,
        entry_point: Option<HnswNodeId>,
        mutation_start_block: u64,
        mutation_end_block: u64,
        mutation_record_count: u64,
    ) {
        let index = usize::from(self.segment_count);
        if index >= HNSW_MAX_SEGMENTS {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "HNSW immutable segment directory is full; compaction is required",
            );
        }
        let mutation_absent = mutation_start_block == u64::MAX && mutation_end_block == u64::MAX;
        let mutation_present = mutation_start_block < mutation_end_block;
        if !mutation_absent && !mutation_present {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "cannot publish an HNSW segment with an invalid mutation extent",
            );
        }
        if graph_nodes > 0 && start_block >= end_block {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "cannot publish a non-empty HNSW segment with an empty graph extent",
            );
        }
        let segment_id = self.next_segment_id.max(1);
        self.segments[index] = HnswSegmentMeta {
            segment_id,
            generation,
            start_block,
            end_block,
            graph_nodes,
            entry_node_id: entry_point.map_or(u64::MAX, |node| node.get() as u64),
            mutation_generation: if mutation_present {
                generation
            } else {
                u64::MAX
            },
            mutation_start_block,
            mutation_end_block,
            mutation_record_count,
        };
        self.segment_count = self.segment_count.saturating_add(1);
        self.next_segment_id = segment_id.saturating_add(1);
        self.graph_nodes = self.graph_nodes.saturating_add(graph_nodes);
        self.record_directory_mutation();
    }

    fn replace_segment_pair(
        &mut self,
        first: usize,
        generation: u64,
        start_block: u64,
        end_block: u64,
        graph_nodes: u64,
        entry_point: Option<HnswNodeId>,
        mutation_start_block: u64,
        mutation_end_block: u64,
        mutation_record_count: u64,
    ) {
        let count = usize::from(self.segment_count);
        if first + 1 >= count {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "bounded HNSW compaction pair is outside the directory",
            );
        }
        let retained_id = self.segments[first].segment_id;
        self.segments[first] = HnswSegmentMeta {
            segment_id: retained_id,
            generation,
            start_block,
            end_block,
            graph_nodes,
            entry_node_id: entry_point.map_or(u64::MAX, |node| node.get() as u64),
            mutation_generation: if mutation_start_block == u64::MAX {
                u64::MAX
            } else {
                generation
            },
            mutation_start_block,
            mutation_end_block,
            mutation_record_count,
        };
        let mut index = first + 1;
        while index + 1 < count {
            self.segments[index] = self.segments[index + 1];
            index += 1;
        }
        self.segments[count - 1] = HnswSegmentMeta::EMPTY;
        self.segment_count = self.segment_count.saturating_sub(1);
        self.graph_nodes = self
            .segments()
            .iter()
            .fold(0_u64, |total, segment| {
                total.saturating_add(segment.graph_nodes)
            });
        self.record_directory_mutation();
    }

    /// Relocates the unchanged active delta as part of an already-counted
    /// directory publication. Pair compaction uses this after writing graph
    /// pages beyond the old delta so future appends cannot create a logical
    /// delta extent that spans immutable graph pages.
    fn relocate_active_delta(
        &mut self,
        start_block: u64,
        end_block: u64,
        generation: u64,
        record_count: u64,
    ) {
        if start_block > end_block || generation == 0 {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "cannot publish an invalid relocated HNSW active delta extent",
            );
        }
        self.delta_start_block = start_block;
        self.delta_end_block = end_block;
        self.delta_generation = generation;
        self.delta_record_count = record_count;
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
            && self.segment_count as usize <= HNSW_MAX_SEGMENTS
            && self.segment_reserved16 == 0
            && self.segment_reserved32 == 0
            && self.next_segment_id != 0
            && ((self.delta_start_block == u64::MAX
                && self.delta_end_block == u64::MAX
                && self.delta_record_count == 0)
                || (self.delta_start_block != u64::MAX
                    && self.delta_start_block <= self.delta_end_block
                    && self.delta_generation != 0
                    && (self.delta_record_count == 0
                        || self.delta_start_block < self.delta_end_block)))
            && self.segment_directory_is_valid()
    }

    const fn segment_directory_is_valid(self) -> bool {
        let mut index = 0;
        let mut prior_id = 0;
        while index < HNSW_MAX_SEGMENTS {
            let segment = self.segments[index];
            if index < self.segment_count as usize {
                if segment.is_empty()
                    || segment.segment_id <= prior_id
                    || segment.generation == 0
                    || segment.start_block < HNSW_FIRST_VECTOR_BLOCK as u64
                    || (segment.graph_nodes > 0 && segment.start_block >= segment.end_block)
                    || (segment.graph_nodes == 0 && segment.start_block != segment.end_block)
                    || (segment.graph_nodes == 0 && segment.entry_node_id != u64::MAX)
                    || (segment.graph_nodes > 0 && segment.entry_node_id >= segment.graph_nodes)
                    || ((segment.mutation_start_block == u64::MAX)
                        != (segment.mutation_end_block == u64::MAX))
                    || ((segment.mutation_start_block == u64::MAX)
                        != (segment.mutation_generation == u64::MAX))
                    || ((segment.mutation_start_block == u64::MAX)
                        != (segment.mutation_record_count == 0))
                    || (segment.mutation_start_block != u64::MAX
                        && (segment.mutation_start_block >= segment.mutation_end_block
                            || segment.mutation_generation == 0))
                    || Self::ranges_overlap(
                        segment.start_block,
                        segment.end_block,
                        segment.mutation_start_block,
                        segment.mutation_end_block,
                    )
                    || Self::ranges_overlap(
                        segment.start_block,
                        segment.end_block,
                        self.delta_start_block,
                        self.delta_end_block,
                    )
                    || Self::ranges_overlap(
                        segment.mutation_start_block,
                        segment.mutation_end_block,
                        self.delta_start_block,
                        self.delta_end_block,
                    )
                {
                    return false;
                }
                prior_id = segment.segment_id;
            } else if !segment.is_empty() {
                return false;
            }
            index += 1;
        }
        let mut left = 0;
        while left < self.segment_count as usize {
            let mut right = left + 1;
            while right < self.segment_count as usize {
                let a = self.segments[left];
                let b = self.segments[right];
                if a.start_block < a.end_block
                    && b.start_block < b.end_block
                    && a.start_block < b.end_block
                    && b.start_block < a.end_block
                {
                    return false;
                }
                if Self::ranges_overlap(
                    a.start_block,
                    a.end_block,
                    b.mutation_start_block,
                    b.mutation_end_block,
                ) || Self::ranges_overlap(
                    a.mutation_start_block,
                    a.mutation_end_block,
                    b.start_block,
                    b.end_block,
                ) || Self::ranges_overlap(
                    a.mutation_start_block,
                    a.mutation_end_block,
                    b.mutation_start_block,
                    b.mutation_end_block,
                ) {
                    return false;
                }
                right += 1;
            }
            left += 1;
        }
        true
    }

    const fn ranges_overlap(a_start: u64, a_end: u64, b_start: u64, b_end: u64) -> bool {
        a_start != u64::MAX
            && b_start != u64::MAX
            && a_start < a_end
            && b_start < b_end
            && a_start < b_end
            && b_start < a_end
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
        self.codec_config_revision = self.codec_spec().map_or(0, |spec| spec.revision().get());
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

    #[cfg(any(test, feature = "pg_test"))]
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

    /// Returns the first block a base-graph read may visit.
    ///
    /// Only the start is published here because historical full-compaction
    /// generations may leave inert pages beyond the selected base. The
    /// immutable segment directory provides exact graph extents to current
    /// serving paths. This broad bound can contain pages that are
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
        self.delta_end_block = block_count;
        self.delta_generation = self
            .next_segment_generation()
            .max(self.delta_generation.saturating_add(1));
        self.delta_record_count = 0;
    }

    /// Records one appended delta record (live or tombstone).
    fn record_delta_append(&mut self, end_block: u64) {
        if end_block < self.delta_start_block {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "HNSW active delta end precedes its start",
            );
        }
        self.delta_end_block = end_block;
        self.delta_record_count = self.delta_record_count.saturating_add(1);
    }

    /// Returns `true` when the delta region is open and has not yet reached
    /// `limit` records.
    const fn delta_accepts_insert(self, limit: u64) -> bool {
        self.delta_start_block != u64::MAX && self.delta_record_count < limit
    }

    fn record_quantization(&mut self, metadata: options::HnswQuantizationMetadata) {
        self.quantization_mode = metadata.mode;
        self.quantization_metadata_version = metadata.version;
        self.scalar_min_bits = metadata.scalar_min_bits;
        self.scalar_max_bits = metadata.scalar_max_bits;
        self.scalar_levels = metadata.scalar_levels;
        self.pq_subvector_dimensions = metadata.pq_subvector_dimensions;
        self.codec_config_revision = metadata.codec_config_revision;
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
