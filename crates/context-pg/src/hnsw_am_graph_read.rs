// Page-backed `PgHnswGraphRead` fragment included by `hnsw_am.rs`: the
// buffer-pinned reader with its pack/attach cache ladder
// (`load_packed`) behind the CP3 packed-store seam.


/// PostgreSQL-page implementation of the pure incremental graph-read port.
///
/// The adapter owns no PostgreSQL buffer beyond an individual port call. Each
/// returned node and neighbor list is copied while its shared buffer lock is
/// held, then the buffer is released before control returns to the search.
struct PgHnswGraphRead {
    index_relation: pg_sys::Relation,
    segment: Option<HnswSegmentMeta>,
    metadata: Option<HnswMetaPage>,
    metadata_lsn: Option<pg_sys::XLogRecPtr>,
    directory: Option<Rc<HnswDirectoryIndex>>,
    packed: Option<HnswPackedGeneration>,
    nodes: BTreeMap<usize, HnswVectorRecord>,
    page_visits: usize,
    node_reads: usize,
    prepared_query: Option<PreparedQuantizedQuery>,
}

/// Pure, owned traversal adapter admitted to segment worker threads only
/// after the PostgreSQL backend has copied the complete immutable segment.
/// It contains no relation, buffer, memory-context, mapped-file, or DSM state.
struct ParallelPackedGraphRead {
    graph: Arc<PackedHnswGraph>,
    metadata: GraphMetadata,
    node_reads: usize,
    prepared_query: Option<PreparedQuantizedQuery>,
}

impl ParallelPackedGraphRead {
    fn new(
        graph: Arc<PackedHnswGraph>,
        segment: HnswSegmentMeta,
        dimensions: usize,
    ) -> context_index::GraphResult<Self> {
        let node_count = usize::try_from(segment.graph_nodes).map_err(|_| {
            context_index::GraphError::CapacityExceeded {
                operation: "parallel HNSW segment nodes",
            }
        })?;
        let entry = if segment.entry_node_id == u64::MAX {
            None
        } else {
            Some(HnswNodeId::new(
                usize::try_from(segment.entry_node_id).map_err(|_| {
                    context_index::GraphError::CapacityExceeded {
                        operation: "parallel HNSW segment entry",
                    }
                })?,
            ))
        };
        Ok(Self {
            graph,
            metadata: GraphMetadata::new(node_count, entry, Some(dimensions))?,
            node_reads: 0,
            prepared_query: None,
        })
    }
}

/// Adapter-local bridge from pure traversal checkpoints to PostgreSQL cancel
/// processing. No PostgreSQL type crosses the `context-index` port boundary.
struct PgHnswCancellation;

impl HnswCancellation for PgHnswCancellation {
    fn check(&mut self) -> context_index::Result<()> {
        // Graph traversal invokes this only outside Generic-WAL and
        // buffer-critical sections, so PostgreSQL may process cancellation.
        pg_sys::check_for_interrupts!();
        Ok(())
    }
}

impl PgHnswGraphRead {
    fn new(index_relation: pg_sys::Relation) -> Self {
        Self {
            index_relation,
            segment: None,
            metadata: None,
            metadata_lsn: None,
            directory: None,
            packed: None,
            nodes: BTreeMap::new(),
            page_visits: 0,
            node_reads: 0,
            prepared_query: None,
        }
    }

    fn for_segment(index_relation: pg_sys::Relation, segment: HnswSegmentMeta) -> Self {
        Self {
            segment: Some(segment),
            ..Self::new(index_relation)
        }
    }

    unsafe fn parallel_local_read(
        &mut self,
    ) -> context_index::GraphResult<Option<ParallelPackedGraphRead>> {
        // SAFETY: the active backend owns the live relation while it copies or
        // retrieves the immutable local packed generation.
        let meta = unsafe { self.meta() };
        // SAFETY: this adapter was constructed with a validated descriptor.
        let Some(segment) = (unsafe { self.selected_segment() }) else {
            return Ok(None);
        };
        // SAFETY: load_packed performs every PostgreSQL operation here, before
        // any returned owned adapter crosses to a worker thread.
        let Some(packed) = (unsafe { self.load_packed()? }) else {
            return Ok(None);
        };
        let Some(graph) = packed.parallel_local_graph() else {
            return Ok(None);
        };
        ParallelPackedGraphRead::new(
            graph,
            segment,
            usize::try_from(meta.dimensions).map_err(|_| {
                context_index::GraphError::CapacityExceeded {
                    operation: "parallel HNSW dimensions",
                }
            })?,
        )
        .map(Some)
    }

    unsafe fn selected_segment(&mut self) -> Option<HnswSegmentMeta> {
        if let Some(segment) = self.segment {
            return Some(segment);
        }
        // SAFETY: the adapter owns the relation for this scan.
        unsafe { self.meta() }.primary_segment()
    }

    unsafe fn meta(&mut self) -> HnswMetaPage {
        if let Some(meta) = self.metadata {
            return meta;
        }
        // SAFETY: the access-method callback owns the relation for this read.
        let buffer = unsafe {
            pg_sys::ReadBufferExtended(
                self.index_relation,
                pg_sys::ForkNumber::MAIN_FORKNUM,
                0,
                pg_sys::ReadBufferMode::RBM_NORMAL,
                ptr::null_mut(),
            )
        };
        // SAFETY: the buffer stays pinned and share-locked while its metapage
        // item is copied into this plain value.
        unsafe {
            pg_sys::LockBuffer(buffer, pg_sys::BUFFER_LOCK_SHARE.cast_signed());
            let page = pg_sys::BufferGetPage(buffer);
            let meta_lsn = pg_sys::PageGetLSN(page);
            let meta = match read_hnsw_meta_page(page) {
                Ok(Some(meta)) => meta,
                Ok(None) => {
                    pg_sys::UnlockReleaseBuffer(buffer);
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                        "HNSW metapage is missing",
                    )
                }
                Err(error) => {
                    pg_sys::UnlockReleaseBuffer(buffer);
                    raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, error)
                }
            };
            pg_sys::UnlockReleaseBuffer(buffer);
            self.metadata = Some(meta);
            self.metadata_lsn = Some(meta_lsn);
            meta
        }
    }

    unsafe fn load_directory(&mut self) {
        if self.directory.is_some() {
            return;
        }
        // SAFETY: the descriptor is copied from the validated metapage.
        let Some(segment) = (unsafe { self.selected_segment() }) else {
            self.directory = Some(Rc::new(HnswDirectoryIndex::default()));
            return;
        };
        // SAFETY: the relation cache entry is live for the current scan.
        let index_oid = unsafe { (*self.index_relation).rd_id.to_u32() };
        // SAFETY: the relation cache entry remains live for this scan.
        let rel_file_number = unsafe { (*self.index_relation).rd_locator.relNumber.to_u32() };
        let cache_key = HnswSegmentCacheKey {
            index_oid,
            rel_file_number,
            segment_id: segment.segment_id,
            generation: segment.generation,
        };
        if let Some(cached) = HNSW_DIRECTORY_CACHE.with(|cache| {
            cache.borrow().get(&cache_key).cloned()
        }) {
            self.directory = Some(cached.directory);
            return;
        }
        // SAFETY: relation metadata is valid for the current AM callback.
        let block_count = unsafe {
            pg_sys::RelationGetNumberOfBlocksInFork(
                self.index_relation,
                pg_sys::ForkNumber::MAIN_FORKNUM,
            )
        };
        let mut directory = HnswDirectoryIndex::default();
        let start_block = block_number_from_u64(segment.start_block, "HNSW segment start block");
        let end_block = block_number_from_u64(segment.end_block, "HNSW segment end block");
        if start_block >= end_block || end_block > block_count {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "HNSW segment directory extent is outside the index relation",
            );
        }
        for block_number in start_block..end_block {
            self.page_visits = self.page_visits.saturating_add(1);
            pg_sys::check_for_interrupts!();
            // SAFETY: the block is within the current main-fork block count and
            // the returned buffer is released before the next iteration.
            let buffer = unsafe {
                pg_sys::ReadBufferExtended(
                    self.index_relation,
                    pg_sys::ForkNumber::MAIN_FORKNUM,
                    block_number,
                    pg_sys::ReadBufferMode::RBM_NORMAL,
                    ptr::null_mut(),
                )
            };
            // SAFETY: every item is copied while the buffer remains pinned and
            // share-locked. No PostgreSQL pointer escapes this block.
            let items = unsafe {
                pg_sys::LockBuffer(buffer, pg_sys::BUFFER_LOCK_SHARE.cast_signed());
                let page = pg_sys::BufferGetPage(buffer);
                if pg_sys::PageIsNew(page)
                    || pg_sys::PageGetMaxOffsetNumber(page) < HNSW_FIRST_OFFSET
                {
                    pg_sys::UnlockReleaseBuffer(buffer);
                    continue;
                }
                let header_item = copy_hnsw_page_item(page, HNSW_FIRST_OFFSET)
                    .unwrap_or_else(|error| {
                        pg_sys::UnlockReleaseBuffer(buffer);
                        raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, error)
                    });
                let header = decode_page_header(&header_item).unwrap_or_else(|error| {
                    pg_sys::UnlockReleaseBuffer(buffer);
                    raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, error.to_string())
                });
                if header.page_id != u64::from(block_number) {
                    pg_sys::UnlockReleaseBuffer(buffer);
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                        "HNSW page header does not match its physical page",
                    );
                }
                if header.kind != GraphPageKind::Directory {
                    pg_sys::UnlockReleaseBuffer(buffer);
                    continue;
                }
                // Locators from another base generation must not enter the
                // index. Directory records resolve by highest revision, and
                // revision is derived from the block number, so the newest
                // pages in the relation always win — which after an
                // interrupted compaction are its orphaned, never-published
                // ones. Serving a node through those returns the wrong row.
                //
                // After a *successful* compaction the same rule happens to
                // pick the right pages, since the fresh base also sits at the
                // highest blocks. That is a coincidence of layout, not a
                // guarantee, and it is not what makes this correct.
                if header.generation != segment.generation {
                    pg_sys::UnlockReleaseBuffer(buffer);
                    continue;
                }
                let max_offset = pg_sys::PageGetMaxOffsetNumber(page);
                let mut items = Vec::with_capacity(
                    usize::from(max_offset.saturating_sub(HNSW_FIRST_OFFSET)),
                );
                for offset in HNSW_FIRST_VECTOR_RECORD_OFFSET..=max_offset {
                    items.push(copy_hnsw_page_item(page, offset).unwrap_or_else(|error| {
                        pg_sys::UnlockReleaseBuffer(buffer);
                        raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, error)
                    }));
                }
                pg_sys::UnlockReleaseBuffer(buffer);
                items
            };
            for item in items {
                let record = decode_hnsw_directory_record(&item).unwrap_or_else(|reason| {
                    raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, reason)
                });
                directory.observe(record);
            }
        }
        let directory = Rc::new(directory);
        HNSW_DIRECTORY_CACHE.with(|cache| {
            cache.borrow_mut().insert(
                cache_key,
                CachedHnswDirectory {
                    directory: Rc::clone(&directory),
                },
            );
        });
        self.directory = Some(directory);
    }

    /// Returns the packed generation to serve traversal from, or `None` when
    /// `pgcontext.hnsw_pack_on_first_use` is off and no pack is already
    /// available anywhere (local cache, delta patch, or the shared
    /// registry) — the caller falls back to unpacked directory reads
    /// instead of paying a full pack inline. Nothing is cached
    /// on a `None` return, so the next call always re-checks.
    unsafe fn load_packed(&mut self) -> context_index::GraphResult<Option<HnswPackedGeneration>> {
        if let Some(graph) = &self.packed {
            return Ok(Some(graph.clone()));
        }
        // SAFETY: the adapter owns the live relation and metapage snapshot.
        let meta = unsafe { self.meta() };
        // SAFETY: the descriptor is copied from the validated metapage.
        let Some(segment) = (unsafe { self.selected_segment() }) else {
            return Ok(None);
        };
        let meta_lsn = self.metadata_lsn.unwrap_or_default();
        // SAFETY: the relation cache entry remains live for this scan.
        let index_oid = unsafe { (*self.index_relation).rd_id.to_u32() };
        // SAFETY: the relation cache entry remains live for this scan.
        let rel_file_number = unsafe { (*self.index_relation).rd_locator.relNumber.to_u32() };
        let cache_key = HnswSegmentCacheKey {
            index_oid,
            rel_file_number,
            segment_id: segment.segment_id,
            generation: segment.generation,
        };
        // SAFETY: `MyDatabaseId` is initialized before index scans and remains
        // stable for this backend.
        let database_oid = unsafe { pg_sys::MyDatabaseId.to_u32() };
        // A pack built from a different physical relation file is not stale —
        // it is a pack of a *different index* that happens to share the OID.
        // REINDEX swaps the relfilenode, and the fresh build's directory
        // revisions restart low enough that the stale-patch path below would
        // see no drift and re-serve the pre-REINDEX graph. Dropping the entry
        // here protects every path after this point by construction.
        let cached_entry = HNSW_PACKED_GRAPH_CACHE.with(|cache| {
            cache
                .borrow()
                .get(&cache_key)
                .filter(|cached| {
                    cached.rel_file_number == rel_file_number
                        && cached.graph.matches_codec(meta)
                })
                .cloned()
        });
        if let Some(cached) = &cached_entry {
            record_hnsw_pack_reuse();
            self.packed = Some(cached.graph.clone());
            return Ok(Some(cached.graph.clone()));
        }

        let mapped_identity = hnsw_mapped_identity(
            database_oid,
            index_oid,
            rel_file_number,
            segment.segment_id,
            segment.generation,
            segment.generation,
            0,
        );
        // SAFETY: the live relation cache entry owns an initialized pg_class
        // form for the duration of this scan.
        let is_temporary = unsafe {
            u8::try_from((*(*self.index_relation).rd_rel).relpersistence).ok()
                == Some(pg_sys::RELPERSISTENCE_TEMP)
        };
        // Temporary indexes are backend-private and disappear at session
        // teardown, which has no top-level commit callback. Their local packed
        // cache already serves repeated scans, so never materialize a mapped
        // filesystem generation that could outlive the temporary relation.
        let mapped_enabled =
            crate::settings::hnsw_mmap_serving_enabled_from_guc() && !is_temporary;
        if mapped_enabled
            && let Some(image) = attach_mapped_packed_image(
                mapped_identity,
                crate::settings::hnsw_mmap_serving_budget_bytes_from_guc(),
            )
        {
            record_hnsw_mapped_attach();
            let graph = HnswPackedGeneration {
                base: PackedGraphStore::Mapped(Rc::new(image)),
            };
            if !graph.matches_codec(meta) {
                record_hnsw_mapped_publish(false);
            } else {
            HNSW_PACKED_GRAPH_CACHE.with(|cache| {
                let mut cache = cache.borrow_mut();
                if cache.len() >= 4 {
                    cache.clear();
                }
                cache.insert(
                    cache_key,
                    CachedPackedHnswGraph {
                        rel_file_number,
                        graph: graph.clone(),
                    },
                );
            });
            self.packed = Some(graph.clone());
            return Ok(Some(graph));
            }
        }

        let shared_enabled = crate::settings::hnsw_shared_serving_enabled_from_guc();
        if shared_enabled {
            pgrx::debug1!(
                "pgcontext shared-attach lookup db={database_oid} index={index_oid} epoch={} meta_lsn={meta_lsn}",
                meta.directory_epoch
            );
            if let Some(image) = attach_shared_image(
                database_oid,
                index_oid,
                rel_file_number,
                segment.segment_id,
                segment.generation,
                segment.generation,
                0,
            )
            {
                record_hnsw_shared_attach();
                let graph = HnswPackedGeneration {
                    base: PackedGraphStore::Shared(Rc::new(image)),
                };
                if !graph.matches_codec(meta) {
                    record_hnsw_shared_publish(false);
                } else {
                HNSW_PACKED_GRAPH_CACHE.with(|cache| {
                    let mut cache = cache.borrow_mut();
                    if cache.len() >= 4 {
                        cache.clear();
                    }
                    cache.insert(
                        cache_key,
                        CachedPackedHnswGraph {
                            rel_file_number,
                            graph: graph.clone(),
                        },
                    );
                });
                self.packed = Some(graph.clone());
                return Ok(Some(graph));
                }
            }
        }

        if !crate::settings::hnsw_pack_on_first_use_from_guc()
            && meta.quantization_mode == options::HNSW_QUANTIZATION_NONE_U16
        {
            // No pack is available anywhere and inline packing is disabled:
            // serve this query from unpacked directory reads instead of
            // paying the full pack cost synchronously. Caches nothing, so
            // the next query (in this backend or another) gets a fresh
            // chance to find a pack that another backend published meanwhile.
            record_hnsw_page_native_fallback();
            return Ok(None);
        }

        let pack_started = std::time::Instant::now();
        let node_count = usize::try_from(segment.graph_nodes).map_err(|_| {
            context_index::GraphError::CapacityExceeded {
                operation: "packed HNSW graph nodes",
            }
        })?;
        let dimensions = usize::try_from(meta.dimensions).map_err(|_| {
            context_index::GraphError::CapacityExceeded {
                operation: "packed HNSW dimensions",
            }
        })?;
        if meta.quantization_mode != options::HNSW_QUANTIZATION_NONE_U16 {
            let projected = projected_packed_segment_bytes(meta, segment).ok_or(
                context_index::GraphError::CapacityExceeded {
                    operation: "quantized HNSW serving projection",
                },
            )?;
            let budget = crate::settings::hnsw_shared_serving_budget_bytes_from_guc();
            if !hnsw_packed_projection_admitted(projected, budget) {
                return Err(context_index::GraphError::AdapterFailure {
                    operation: "pack quantized HNSW generation",
                    message: format!(
                        "projected peak memory {projected} bytes exceeds pgcontext.hnsw_shared_serving_budget_mb budget {budget} bytes"
                    ),
                });
            }
        }
        // SAFETY: admission above completed before any complete-segment
        // allocation. Pages are now copied and decoded while individually
        // pinned; the returned records own their vectors and links.
        let records = unsafe { read_hnsw_segment_records(self.index_relation, segment) };
        let local_graph = PackedHnswGraph::from_records(
            records,
            node_count,
            dimensions,
            meta.codec_spec(),
        )?;
        if meta.quantization_mode != options::HNSW_QUANTIZATION_NONE_U16
            && local_graph.byte_size()
                > crate::settings::hnsw_shared_serving_budget_bytes_from_guc()
        {
            return Err(context_index::GraphError::AdapterFailure {
                operation: "pack quantized HNSW generation",
                message: "actual packed generation exceeds the serving memory budget".to_owned(),
            });
        }
        record_hnsw_pack_build(
            local_graph.byte_size(),
            u64::try_from(pack_started.elapsed().as_millis()).unwrap_or(u64::MAX),
        );
        // SAFETY: this adapter owns the live relation for the current scan.
        unsafe { self.load_directory() };
        if shared_enabled
            && let Ok(image_bytes) = local_graph.encode_image()
        {
            let budget = crate::settings::hnsw_shared_serving_budget_bytes_from_guc();
            pgrx::debug1!(
                "pgcontext shared-publish db={database_oid} index={index_oid} epoch={} meta_lsn={meta_lsn} bytes={}",
                meta.directory_epoch,
                image_bytes.len()
            );
            let published = publish_packed_image(
                database_oid,
                index_oid,
                rel_file_number,
                segment.segment_id,
                segment.generation,
                segment.generation,
                0,
                &image_bytes,
                budget,
            );
            record_hnsw_shared_publish(published);
        }
        if mapped_enabled {
            let published = local_graph.encode_image().is_ok_and(|image_bytes| {
                publish_mapped_packed_image(
                    mapped_identity,
                    &image_bytes,
                    crate::settings::hnsw_mmap_serving_budget_bytes_from_guc(),
                )
            });
            record_hnsw_mapped_publish(published);
        }
        let graph = HnswPackedGeneration {
            base: PackedGraphStore::Local(Arc::new(local_graph)),
        };
        HNSW_PACKED_GRAPH_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.len() >= 4 {
                cache.clear();
            }
            cache.insert(
                cache_key,
                CachedPackedHnswGraph {
                    rel_file_number,
                    graph: graph.clone(),
                },
            );
        });
        self.packed = Some(graph.clone());
        Ok(Some(graph))
    }

    unsafe fn with_node_item<R>(
        &mut self,
        wanted: HnswNodeId,
        visitor: impl FnOnce(&storage::HnswVectorRecordView<'_>) -> R,
    ) -> Option<R> {
        // SAFETY: the callback owns the live relation for this complete scan.
        unsafe { self.load_directory() };
        let locator = self.directory.as_ref()?.node(wanted)?;
        let block_number = pg_sys::BlockNumber::try_from(locator.target_page).unwrap_or_else(|_| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "HNSW directory target page exceeds PostgreSQL block storage",
            )
        });
        let slot = pg_sys::OffsetNumber::try_from(locator.target_slot).unwrap_or_else(|_| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "HNSW directory target slot exceeds PostgreSQL offset storage",
            )
        });
        if slot < HNSW_FIRST_VECTOR_RECORD_OFFSET {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "HNSW directory target slot points outside the record region",
            );
        }
        // SAFETY: relation metadata is valid for the current AM callback.
        let block_count = unsafe {
            pg_sys::RelationGetNumberOfBlocksInFork(
                self.index_relation,
                pg_sys::ForkNumber::MAIN_FORKNUM,
            )
        };
        if block_number < HNSW_FIRST_VECTOR_BLOCK || block_number >= block_count {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "HNSW directory target page is outside the index relation",
            );
        }
        self.page_visits = self.page_visits.saturating_add(1);
        pg_sys::check_for_interrupts!();
        // SAFETY: the validated target block is within the current relation.
        let buffer = unsafe {
            pg_sys::ReadBufferExtended(
                self.index_relation,
                pg_sys::ForkNumber::MAIN_FORKNUM,
                block_number,
                pg_sys::ReadBufferMode::RBM_NORMAL,
                ptr::null_mut(),
            )
        };
        // SAFETY: both items are borrowed only while the target buffer remains
        // pinned and share-locked. The visitor cannot return either borrow.
        unsafe {
            pg_sys::LockBuffer(buffer, pg_sys::BUFFER_LOCK_SHARE.cast_signed());
            let page = pg_sys::BufferGetPage(buffer);
            if pg_sys::PageIsNew(page) || pg_sys::PageGetMaxOffsetNumber(page) < slot {
                pg_sys::UnlockReleaseBuffer(buffer);
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    "HNSW directory target slot is missing",
                );
            }
            let (header_pointer, header_len) =
                checked_hnsw_page_item_span(page, HNSW_FIRST_OFFSET).unwrap_or_else(|error| {
                    pg_sys::UnlockReleaseBuffer(buffer);
                    raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, error)
                });
            let header_item = slice::from_raw_parts(header_pointer, header_len);
            let header = decode_page_header(header_item).unwrap_or_else(|error| {
                pg_sys::UnlockReleaseBuffer(buffer);
                raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, error.to_string())
            });
            if header.kind != GraphPageKind::Node || header.page_id != locator.target_page {
                pg_sys::UnlockReleaseBuffer(buffer);
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    "HNSW directory locator does not reference a node page",
                );
            }
            let (record_pointer, record_len) =
                checked_hnsw_page_item_span(page, slot).unwrap_or_else(|error| {
                    pg_sys::UnlockReleaseBuffer(buffer);
                    raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, error)
                });
            let record = hnsw_vector_record_view(record_pointer, record_len);
            if record.node_id() != wanted {
                pg_sys::UnlockReleaseBuffer(buffer);
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    "HNSW directory locator references the wrong node",
                );
            }
            self.node_reads = self.node_reads.saturating_add(1);
            let result = visitor(&record);
            pg_sys::UnlockReleaseBuffer(buffer);
            Some(result)
        }
    }

    unsafe fn node(&mut self, wanted: HnswNodeId) -> Option<HnswVectorRecord> {
        if let Some(record) = self.nodes.get(&wanted.get()) {
            return Some(record.clone());
        }
        // SAFETY: the callback owns the relation and the visitor materializes
        // all bytes before `with_node_item` releases its page lock.
        let record = unsafe {
            self.with_node_item(wanted, |view| {
                let vector = DenseVector::new(view.vector().to_vec())
                    .unwrap_or_else(|error| raise_core_error(error));
                let mut layers = Vec::with_capacity(view.layer_count());
                for layer_index in 0..view.layer_count() {
                    let mut neighbors = Vec::new();
                    let present =
                        view.read_neighbors_into(LayerIndex::new(layer_index), &mut neighbors);
                    debug_assert!(present);
                    layers.push(neighbors);
                }
                HnswVectorRecord {
                    node_id: view.node_id(),
                    heap_tid: view.heap_tid(),
                    vector,
                    base_neighbors: layers[0].clone(),
                    layers,
                }
            })
        }?;
        self.nodes.insert(wanted.get(), record.clone());
        Some(record)
    }
}

const fn hnsw_packed_projection_admitted(projected: u64, budget: u64) -> bool {
    projected <= budget
}
