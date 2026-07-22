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
    metadata: Option<HnswMetaPage>,
    metadata_lsn: Option<pg_sys::XLogRecPtr>,
    directory: Option<Rc<HnswDirectoryIndex>>,
    packed: Option<HnswPackedGeneration>,
    nodes: BTreeMap<usize, HnswVectorRecord>,
    page_visits: usize,
    node_reads: usize,
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
            metadata: None,
            metadata_lsn: None,
            directory: None,
            packed: None,
            nodes: BTreeMap::new(),
            page_visits: 0,
            node_reads: 0,
        }
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
        // SAFETY: this adapter owns the live relation for the current scan.
        let meta = unsafe { self.meta() };
        let meta_lsn = self.metadata_lsn.unwrap_or_default();
        // SAFETY: the relation cache entry is live for the current scan.
        let index_oid = unsafe { (*self.index_relation).rd_id.to_u32() };
        if let Some(cached) = HNSW_DIRECTORY_CACHE.with(|cache| {
            cache.borrow().get(&index_oid).filter(|cached| {
                cached.epoch == meta.directory_epoch && cached.meta_lsn == meta_lsn
            }).cloned()
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
        for block_number in HNSW_FIRST_VECTOR_BLOCK..block_count {
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
                if header.generation != meta.base_generation {
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
                index_oid,
                CachedHnswDirectory {
                    epoch: meta.directory_epoch,
                    meta_lsn,
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
        let meta_lsn = self.metadata_lsn.unwrap_or_default();
        // SAFETY: the relation cache entry remains live for this scan.
        let index_oid = unsafe { (*self.index_relation).rd_id.to_u32() };
        // SAFETY: the relation cache entry remains live for this scan.
        let rel_file_number = unsafe { (*self.index_relation).rd_locator.relNumber.to_u32() };
        // A pack built from a different physical relation file is not stale —
        // it is a pack of a *different index* that happens to share the OID.
        // REINDEX swaps the relfilenode, and the fresh build's directory
        // revisions restart low enough that the stale-patch path below would
        // see no drift and re-serve the pre-REINDEX graph. Dropping the entry
        // here protects every path after this point by construction.
        let cached_entry = HNSW_PACKED_GRAPH_CACHE.with(|cache| {
            cache
                .borrow()
                .get(&index_oid)
                .filter(|cached| cached.rel_file_number == rel_file_number)
                .cloned()
        });
        if let Some(cached) = &cached_entry
            && cached.epoch == meta.directory_epoch
            && cached.meta_lsn == meta_lsn
        {
            record_hnsw_pack_reuse();
            self.packed = Some(cached.graph.clone());
            return Ok(Some(cached.graph.clone()));
        }

        // SAFETY: `MyDatabaseId` is initialized before index scans and is
        // stable for this backend.
        let database_oid = unsafe { pg_sys::MyDatabaseId.to_u32() };
        let mapped_identity = hnsw_mapped_identity(
            database_oid,
            index_oid,
            rel_file_number,
            meta.directory_epoch,
            meta_lsn,
        );
        let mapped_enabled = crate::settings::hnsw_mmap_serving_enabled_from_guc();
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
            HNSW_PACKED_GRAPH_CACHE.with(|cache| {
                let mut cache = cache.borrow_mut();
                if cache.len() >= 4 {
                    cache.clear();
                }
                cache.insert(
                    index_oid,
                    CachedPackedHnswGraph {
                        rel_file_number,
                        epoch: meta.directory_epoch,
                        meta_lsn,
                        graph: graph.clone(),
                    },
                );
            });
            self.packed = Some(graph.clone());
            return Ok(Some(graph));
        }

        let shared_enabled = crate::settings::hnsw_shared_serving_enabled_from_guc();
        if shared_enabled {
            pgrx::debug1!(
                "pgcontext shared-attach lookup db={database_oid} index={index_oid} epoch={} meta_lsn={meta_lsn}",
                meta.directory_epoch
            );
            if let Some(image) =
                attach_shared_image(database_oid, index_oid, meta.directory_epoch, meta_lsn)
            {
                record_hnsw_shared_attach();
                let graph = HnswPackedGeneration {
                    base: PackedGraphStore::Shared(Rc::new(image)),
                };
                HNSW_PACKED_GRAPH_CACHE.with(|cache| {
                    let mut cache = cache.borrow_mut();
                    if cache.len() >= 4 {
                        cache.clear();
                    }
                    cache.insert(
                        index_oid,
                        CachedPackedHnswGraph {
                            rel_file_number,
                            epoch: meta.directory_epoch,
                            meta_lsn,
                            graph: graph.clone(),
                        },
                    );
                });
                self.packed = Some(graph.clone());
                return Ok(Some(graph));
            }
        }

        if !crate::settings::hnsw_pack_on_first_use_from_guc() {
            // No pack is available anywhere and inline packing is disabled:
            // serve this query from unpacked directory reads instead of
            // paying the full pack cost synchronously. Caches nothing, so
            // the next query (in this backend or another) gets a fresh
            // chance to find a pack that another backend published meanwhile.
            record_hnsw_page_native_fallback();
            return Ok(None);
        }

        let pack_started = std::time::Instant::now();
        // SAFETY: all relation pages are copied and decoded while individually
        // pinned; the returned records own their vectors and links.
        let records = unsafe { read_hnsw_vector_records(self.index_relation) };
        let node_count = usize::try_from(meta.graph_nodes).map_err(|_| {
            context_index::GraphError::CapacityExceeded {
                operation: "packed HNSW graph nodes",
            }
        })?;
        let dimensions = usize::try_from(meta.dimensions).map_err(|_| {
            context_index::GraphError::CapacityExceeded {
                operation: "packed HNSW dimensions",
            }
        })?;
        let local_graph = PackedHnswGraph::from_records(records, node_count, dimensions)?;
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
                meta.directory_epoch,
                meta_lsn,
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
            base: PackedGraphStore::Local(Rc::new(local_graph)),
        };
        HNSW_PACKED_GRAPH_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.len() >= 4 {
                cache.clear();
            }
            cache.insert(
                index_oid,
                CachedPackedHnswGraph {
                    rel_file_number,
                    epoch: meta.directory_epoch,
                    meta_lsn,
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
