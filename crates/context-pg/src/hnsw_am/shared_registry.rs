// Shared packed-generation base registry (hybrid serving model).
//
// A fixed-size table lives in a `GetNamedDSMSegment` allocation (available
// without `shared_preload_libraries` since PostgreSQL 17) and maps
// `(database oid, index oid)` to the dynamic-shared-memory location of an
// encoded [`context_storage::PackedGraphImageView`] payload: a
// `dsm_handle`, the metapage identity it was built from, and its byte
// length. One backend builds and publishes an image; every other backend
// attaches the published payload directly instead of repacking from
// PostgreSQL pages, removing the per-backend memory duplication and
// cold-start repack cost the backend-local-only cache paid.
//
// The registry slot table and the DSM payload segments are both rebuildable
// cache material: PostgreSQL index pages remain authoritative, a lookup
// miss or a disabled GUC always falls back to the existing backend-local
// pack path, and an over-budget or failed publish is a soft skip, never an
// error.

/// Fixed slot count. A linear scan over this many entries under one LWLock
/// is negligible next to a graph pack/attach; this is not a hot per-query
/// path (attaches are cached in the existing backend-local packed cache).
const HNSW_SHARED_REGISTRY_SLOTS: usize = 64;
const HNSW_SHARED_REGISTRY_NAME: &CStr = c"pgcontext_hnsw_shared_registry";

#[repr(C)]
#[derive(Clone, Copy)]
struct HnswSharedRegistrySlot {
    /// `0` marks an empty slot; PostgreSQL never assigns database oid `0`.
    database_oid: u32,
    index_oid: u32,
    epoch: u64,
    meta_lsn: u64,
    dsm_handle: pg_sys::dsm_handle,
    byte_len: u64,
}

const EMPTY_SLOT: HnswSharedRegistrySlot = HnswSharedRegistrySlot {
    database_oid: 0,
    index_oid: 0,
    epoch: 0,
    meta_lsn: 0,
    dsm_handle: 0,
    byte_len: 0,
};

#[repr(C)]
struct HnswSharedRegistryHeader {
    lock: pg_sys::LWLock,
    tranche_id: i32,
    total_bytes: u64,
    slots: [HnswSharedRegistrySlot; HNSW_SHARED_REGISTRY_SLOTS],
}

/// `GetNamedDSMSegment` init callback: PostgreSQL guarantees this runs
/// exactly once for the segment's lifetime, with concurrent callers in other
/// backends blocked until it returns.
unsafe extern "C-unwind" fn hnsw_shared_registry_init_callback(ptr: *mut c_void) {
    let header = ptr.cast::<HnswSharedRegistryHeader>();
    // SAFETY: PostgreSQL passes exactly `size_of::<HnswSharedRegistryHeader>()`
    // freshly allocated bytes to this one-time initializer; nothing observes
    // this memory before `LWLockInitialize` below installs a valid lock.
    unsafe {
        header.write_bytes(0, 1);
    }
    // SAFETY: `LWLockNewTrancheId` only reads/increments a global shared
    // counter; it has no preconditions beyond PostgreSQL being initialized,
    // which holds inside this callback.
    let tranche_id = unsafe { pg_sys::LWLockNewTrancheId() };
    // SAFETY: `header` points to the writable memory this callback owns
    // exclusively during initialization, and `lock` is the header's first
    // field at a stable, aligned offset.
    unsafe {
        pg_sys::LWLockInitialize(core::ptr::addr_of_mut!((*header).lock), tranche_id);
        core::ptr::addr_of_mut!((*header).tranche_id).write(tranche_id);
    }
}

thread_local! {
    static HNSW_SHARED_REGISTRY_TRANCHE_REGISTERED: Cell<bool> = const { Cell::new(false) };
}

/// Attaches (creating on first call server-wide) the shared registry header.
///
/// # Safety
///
/// The caller must not retain the returned pointer past the current
/// PostgreSQL backend's lifetime, and must serialize all field access
/// through `header.lock` (`LWLockAcquire`/`LWLockRelease`) — the memory is
/// genuinely shared across OS processes, so Rust's aliasing rules do not
/// govern it; the LWLock is the only correctness boundary.
unsafe fn shared_registry_ptr() -> *mut HnswSharedRegistryHeader {
    let mut found = false;
    // SAFETY: the requested size matches `HnswSharedRegistryHeader` exactly,
    // the init callback has a compatible `extern "C-unwind" fn(*mut c_void)`
    // signature, and PostgreSQL keeps the returned mapping valid for the
    // rest of this backend's lifetime.
    let ptr = unsafe {
        pg_sys::GetNamedDSMSegment(
            HNSW_SHARED_REGISTRY_NAME.as_ptr(),
            size_of::<HnswSharedRegistryHeader>(),
            Some(hnsw_shared_registry_init_callback),
            &mut found,
        )
    };
    let header = ptr.cast::<HnswSharedRegistryHeader>();
    let tranche_id =
        // SAFETY: `GetNamedDSMSegment` has returned; the init callback (this
        // call or an earlier one) has already run and set `tranche_id`.
        unsafe { core::ptr::addr_of!((*header).tranche_id).read() };
    HNSW_SHARED_REGISTRY_TRANCHE_REGISTERED.with(|registered| {
        if !registered.get() {
            // SAFETY: registering a tranche name only touches this
            // backend's local wait-event naming table; safe to call
            // repeatedly (guarded here just to avoid redundant calls).
            unsafe {
                pg_sys::LWLockRegisterTranche(tranche_id, c"pgcontext_hnsw_shared".as_ptr());
            }
            registered.set(true);
        }
    });
    header
}

/// Looks up a published shared image identity for `(database_oid,
/// index_oid)` matching `epoch`/`meta_lsn` exactly.
///
/// # Safety
///
/// `header` must be a live pointer obtained from [`shared_registry_ptr`]
/// during the current backend's lifetime.
unsafe fn lookup_shared_slot(
    header: *mut HnswSharedRegistryHeader,
    database_oid: u32,
    index_oid: u32,
    epoch: u64,
    meta_lsn: u64,
) -> Option<HnswSharedRegistrySlot> {
    // SAFETY: `header` is live per this function's contract; `lock` is a
    // stable field of the header PostgreSQL keeps mapped identically in
    // every attached backend.
    unsafe {
        pg_sys::LWLockAcquire(
            core::ptr::addr_of_mut!((*header).lock),
            pg_sys::LWLockMode::LW_SHARED,
        );
    }
    // SAFETY: the lock above excludes concurrent writers; reading the slots
    // array is a plain field read of `Copy` data.
    let found = unsafe {
        (*header)
            .slots
            .iter()
            .copied()
            .find(|slot| {
                slot.database_oid == database_oid
                    && slot.index_oid == index_oid
                    && slot.epoch == epoch
                    && slot.meta_lsn == meta_lsn
            })
    };
    // SAFETY: releases exactly the lock acquired immediately above.
    unsafe {
        pg_sys::LWLockRelease(core::ptr::addr_of_mut!((*header).lock));
    }
    found
}

/// Publishes a new slot for `(database_oid, index_oid)`, replacing any
/// existing entry, subject to the global byte budget. Returns the
/// previously published `dsm_handle` (to unpin after the caller releases
/// its own segment reference) when a slot was replaced, or `None` when this
/// was a fresh slot or the budget rejected the publish (in which case the
/// caller's freshly created segment is the caller's responsibility to
/// unpin/detach unpublished).
///
/// # Safety
///
/// `header` must be a live pointer obtained from [`shared_registry_ptr`]
/// during the current backend's lifetime.
unsafe fn commit_shared_slot(
    header: *mut HnswSharedRegistryHeader,
    database_oid: u32,
    index_oid: u32,
    epoch: u64,
    meta_lsn: u64,
    dsm_handle: pg_sys::dsm_handle,
    byte_len: u64,
    budget_bytes: u64,
) -> Result<Option<pg_sys::dsm_handle>, ()> {
    // SAFETY: see `lookup_shared_slot`.
    unsafe {
        pg_sys::LWLockAcquire(
            core::ptr::addr_of_mut!((*header).lock),
            pg_sys::LWLockMode::LW_EXCLUSIVE,
        );
    }
    // SAFETY: the exclusive lock above excludes every other reader/writer
    // for the remainder of this block.
    let result = unsafe {
        let slots = core::ptr::addr_of_mut!((*header).slots);
        let existing_index = (0..HNSW_SHARED_REGISTRY_SLOTS).find(|&index| {
            let slot = (*slots)[index];
            slot.database_oid == database_oid && slot.index_oid == index_oid
        });
        let empty_index = existing_index.or_else(|| {
            (0..HNSW_SHARED_REGISTRY_SLOTS).find(|&index| (*slots)[index].database_oid == 0)
        });
        let previous_bytes = existing_index.map_or(0, |index| (*slots)[index].byte_len);
        let total = core::ptr::addr_of_mut!((*header).total_bytes);
        let prospective_total = (*total).saturating_sub(previous_bytes) + byte_len;
        match empty_index {
            Some(_) if prospective_total > budget_bytes => Err(()),
            Some(index) => {
                let previous_handle =
                    existing_index.map(|_| (*slots)[index].dsm_handle).filter(|&h| h != 0);
                (*slots)[index] = HnswSharedRegistrySlot {
                    database_oid,
                    index_oid,
                    epoch,
                    meta_lsn,
                    dsm_handle,
                    byte_len,
                };
                *total = prospective_total;
                Ok(previous_handle)
            }
            None => Err(()),
        }
    };
    // SAFETY: releases exactly the lock acquired above.
    unsafe {
        pg_sys::LWLockRelease(core::ptr::addr_of_mut!((*header).lock));
    }
    result
}

/// Removes and returns the slot for `(database_oid, index_oid)` if present,
/// for callers that must retract a publish they cannot complete.
///
/// # Safety
///
/// `header` must be a live pointer obtained from [`shared_registry_ptr`]
/// during the current backend's lifetime.
unsafe fn evict_shared_slot(
    header: *mut HnswSharedRegistryHeader,
    database_oid: u32,
    index_oid: u32,
) -> Option<pg_sys::dsm_handle> {
    // SAFETY: see `lookup_shared_slot`.
    unsafe {
        pg_sys::LWLockAcquire(
            core::ptr::addr_of_mut!((*header).lock),
            pg_sys::LWLockMode::LW_EXCLUSIVE,
        );
    }
    // SAFETY: the exclusive lock above excludes concurrent access.
    let evicted = unsafe {
        let slots = core::ptr::addr_of_mut!((*header).slots);
        (0..HNSW_SHARED_REGISTRY_SLOTS).find_map(|index| {
            let slot = (*slots)[index];
            if slot.database_oid == database_oid && slot.index_oid == index_oid {
                let total = core::ptr::addr_of_mut!((*header).total_bytes);
                *total = (*total).saturating_sub(slot.byte_len);
                (*slots)[index] = EMPTY_SLOT;
                Some(slot.dsm_handle).filter(|&h| h != 0)
            } else {
                None
            }
        })
    };
    // SAFETY: releases exactly the lock acquired above.
    unsafe {
        pg_sys::LWLockRelease(core::ptr::addr_of_mut!((*header).lock));
    }
    evicted
}

/// An attached read view over a published shared packed-graph image.
///
/// Detaches its dynamic shared memory mapping on drop. The view's lifetime
/// parameter is erased to `'static` at construction and never exposed
/// outside `&self` accessors, so it never outlives the owning value that
/// keeps the mapping attached.
pub(crate) struct AttachedSharedImage {
    segment: *mut pg_sys::dsm_segment,
    view: PackedGraphImageView<'static>,
}

impl AttachedSharedImage {
    /// Attaches `handle` and validates it as a packed graph image.
    ///
    /// # Safety
    ///
    /// `handle` must name a `dsm_handle` published by
    /// [`publish_packed_image`] (or an equivalent producer using the same
    /// image codec) whose payload segment has not yet been destroyed.
    unsafe fn new(
        handle: pg_sys::dsm_handle,
        expected_len: u64,
    ) -> Result<Self, PackedGraphImageError> {
        // SAFETY: `handle` is caller-guaranteed to name a live segment.
        let segment = unsafe { pg_sys::dsm_attach(handle) };
        if segment.is_null() {
            return Err(PackedGraphImageError::TruncatedHeader);
        }
        // `dsm_attach` ties the mapping to the current resource owner by
        // default, which would tear it down at the end of this query's
        // transaction even though `AttachedSharedImage` is cached in the
        // backend-local packed-generation cache across transactions.
        // SAFETY: pins this backend's own just-attached mapping so it
        // outlives the current resource owner; `Drop` detaches it later.
        unsafe { pg_sys::dsm_pin_mapping(segment) };
        // `dsm_segment_map_length` reports the segment's rounded-up
        // allocation size, not the exact payload length recorded in the
        // registry, so this only bounds `expected_len` — it must never be
        // compared for equality against it.
        // SAFETY: `segment` is the just-attached, non-null segment.
        let mapped_len = unsafe { pg_sys::dsm_segment_map_length(segment) };
        let Ok(expected_len_usize) = usize::try_from(expected_len) else {
            // SAFETY: releases the mapping this function attached above.
            unsafe { pg_sys::dsm_detach(segment) };
            return Err(PackedGraphImageError::CountOverflow);
        };
        if expected_len_usize > mapped_len {
            // SAFETY: releases the mapping this function attached above.
            unsafe { pg_sys::dsm_detach(segment) };
            return Err(PackedGraphImageError::TruncatedPayload);
        }
        // SAFETY: the mapping is pinned above and stays valid until `Drop`
        // detaches it, and `expected_len_usize <= mapped_len` was just
        // checked, so this slice stays in bounds for its erased lifetime.
        let bytes = unsafe {
            let address = pg_sys::dsm_segment_address(segment).cast::<u8>();
            core::slice::from_raw_parts(address, expected_len_usize)
        };
        match PackedGraphImageView::attach(bytes, false) {
            Ok(view) => Ok(Self {
                segment,
                view,
            }),
            Err(error) => {
                // SAFETY: releases the mapping this function attached above.
                unsafe { pg_sys::dsm_detach(segment) };
                Err(error)
            }
        }
    }

    pub(crate) fn view(&self) -> &PackedGraphImageView<'static> {
        &self.view
    }

}

impl Drop for AttachedSharedImage {
    fn drop(&mut self) {
        // SAFETY: `self.segment` was attached exactly once in `Self::new`
        // and is detached exactly once here.
        unsafe {
            pg_sys::dsm_detach(self.segment);
        }
    }
}

/// Attempts to attach the shared image for `(database_oid, index_oid)`
/// matching `epoch`/`meta_lsn`. Returns `None` on a registry miss, a stale
/// entry, or any attach/validation failure — every path is a safe fallback
/// to the backend-local pack.
pub(crate) fn attach_shared_image(
    database_oid: u32,
    index_oid: u32,
    epoch: u64,
    meta_lsn: u64,
) -> Option<AttachedSharedImage> {
    // SAFETY: `shared_registry_ptr` returns a pointer valid for this
    // backend's lifetime; `lookup_shared_slot` requires exactly that.
    let slot = unsafe {
        let header = shared_registry_ptr();
        lookup_shared_slot(header, database_oid, index_oid, epoch, meta_lsn)
    }?;
    if slot.dsm_handle == 0 {
        return None;
    }
    // SAFETY: `slot.dsm_handle` was read from the registry under lock and
    // names a segment published by `publish_packed_image`, which keeps
    // segments referenced by live registry entries pinned.
    match unsafe {
        AttachedSharedImage::new(slot.dsm_handle, slot.byte_len)
    } {
        Ok(image) => Some(image),
        Err(error) => {
            pgrx::debug1!("pgcontext shared-attach validation failed: {error}");
            // A stale or corrupt entry would otherwise fail every future
            // backend's attach attempt until the next publish; evict it so
            // the registry self-heals instead of staying poisoned.
            evict_shared_image(database_oid, index_oid);
            None
        }
    }
}

/// Encodes `bytes` into a fresh pinned DSM segment and publishes it for
/// `(database_oid, index_oid)`, subject to `budget_bytes`. Returns `true` on
/// a successful publish; `false` means the budget rejected the publish or
/// segment creation failed — the caller continues serving from its local
/// pack either way, so this is advisory, not an error.
pub(crate) fn publish_packed_image(
    database_oid: u32,
    index_oid: u32,
    epoch: u64,
    meta_lsn: u64,
    bytes: &[u8],
    budget_bytes: u64,
) -> bool {
    let byte_len = bytes.len();
    if byte_len == 0 {
        return false;
    }
    #[allow(
        clippy::cast_possible_wrap,
        reason = "DSM_CREATE_* are tiny bit-flag constants far below i32::MAX"
    )]
    const DSM_CREATE_FLAGS: core::ffi::c_int = pg_sys::DSM_CREATE_NULL_IF_MAXSEGMENTS as core::ffi::c_int;
    // SAFETY: `dsm_create` allocates a fresh segment of exactly `byte_len`
    // bytes; `DSM_CREATE_NULL_IF_MAXSEGMENTS` makes exhaustion return null
    // instead of erroring, which this function handles below.
    let segment = unsafe { pg_sys::dsm_create(byte_len, DSM_CREATE_FLAGS) };
    if segment.is_null() {
        return false;
    }
    // SAFETY: `segment` is the just-created, non-null, exclusively-owned
    // segment; `dsm_segment_address` returns a mapping at least `byte_len`
    // bytes long, matching `bytes.len()` exactly.
    unsafe {
        let address = pg_sys::dsm_segment_address(segment).cast::<u8>();
        ptr::copy_nonoverlapping(bytes.as_ptr(), address, bytes.len());
        // Pinning must happen before this backend detaches, or the segment
        // is destroyed as soon as its (sole, so far) referencing backend
        // detaches.
        pg_sys::dsm_pin_segment(segment);
    }
    // SAFETY: `segment` is this backend's own pinned `dsm_create` mapping;
    // reading its handle then detaching only drops the local mapping, which
    // is no longer needed once the handle is recorded in the registry.
    let handle = unsafe {
        let handle = pg_sys::dsm_segment_handle(segment);
        pg_sys::dsm_detach(segment);
        handle
    };
    let byte_len_u64 = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    // SAFETY: see `attach_shared_image`.
    let commit = unsafe {
        let header = shared_registry_ptr();
        commit_shared_slot(
            header,
            database_oid,
            index_oid,
            epoch,
            meta_lsn,
            handle,
            byte_len_u64,
            budget_bytes,
        )
    };
    match commit {
        Ok(previous_handle) => {
            if let Some(previous_handle) = previous_handle {
                // SAFETY: `dsm_unpin_segment` looks up the segment by handle
                // in the shared control table and needs no live mapping;
                // this was the only reference the replaced slot held.
                unsafe {
                    pg_sys::dsm_unpin_segment(previous_handle);
                }
            }
            true
        }
        Err(()) => {
            // The budget rejected this publish; unpin the segment created
            // and pinned above so it does not leak.
            // SAFETY: `handle` names that segment; no registry slot
            // references it (the commit failed before installing it).
            unsafe {
                pg_sys::dsm_unpin_segment(handle);
            }
            false
        }
    }
}

/// Retracts a stale or corrupt registry entry for `(database_oid,
/// index_oid)`, unpinning its segment. Used defensively when an attach
/// fails validation after a lookup hit, so a poisoned entry does not keep
/// failing every subsequent backend's attach attempt.
pub(crate) fn evict_shared_image(database_oid: u32, index_oid: u32) {
    // SAFETY: see `attach_shared_image`.
    let evicted = unsafe {
        let header = shared_registry_ptr();
        evict_shared_slot(header, database_oid, index_oid)
    };
    if let Some(handle) = evicted {
        // SAFETY: `handle` was just removed from the only registry slot
        // that referenced it, matching the contract of `dsm_unpin_segment`
        // described in `publish_packed_image`.
        unsafe {
            pg_sys::dsm_unpin_segment(handle);
        }
    }
}
