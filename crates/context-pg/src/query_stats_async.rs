//! Rollback-independent automatic query telemetry transport.
//!
//! PostgreSQL errors abort the caller's transaction, so automatic observations
//! cannot be written with SPI in the query backend. PostgreSQL 17 introduced
//! the named-DSM registry used here: query backends enqueue a fixed-size event
//! without waiting, and one dynamic background worker per database persists
//! events in independent transactions. Producers acquire the queue lock
//! conditionally and fail open on contention.

use context_query::StageDiagnostic;

/// Backend-local identity that prevents nested queries from finishing each
/// other's observations while PostgreSQL unwinds an error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ObservationToken(u64);

/// Fixed, non-sensitive summary supplied when executor control returns normally.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AutomaticQuerySummary {
    pub(crate) result_count: usize,
    pub(crate) visits: usize,
    pub(crate) filter_candidates: usize,
    pub(crate) candidates: usize,
    pub(crate) rechecks: usize,
    pub(crate) stages: usize,
    pub(crate) expansions: usize,
    pub(crate) completion: &'static str,
    pub(crate) lifecycle: &'static str,
    pub(crate) strategy: &'static str,
}

/// Current-database queue health exposed through a bounded SQL status row.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct QueueSnapshot {
    pub(crate) enqueued: u64,
    pub(crate) persisted: u64,
    pub(crate) dropped_contention: u64,
    pub(crate) dropped_full: u64,
    pub(crate) dropped_orphaned: u64,
    pub(crate) database_slot_exhausted: u64,
    pub(crate) worker_launch_failures: u64,
    pub(crate) pending: u64,
    pub(crate) worker_pid: Option<i32>,
}

/// Backend-local event projection used only by the pgrx wrapper suite.
#[cfg(feature = "pg_test")]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TestEventSnapshot {
    pub(crate) collection_id: i64,
    pub(crate) query_kind: String,
    pub(crate) strategy: String,
    pub(crate) result_count: u64,
    pub(crate) visits: u64,
    pub(crate) filter_candidates: u64,
    pub(crate) candidates: u64,
    pub(crate) rechecks: u64,
    pub(crate) stages: u64,
    pub(crate) expansions: u64,
    pub(crate) completion: String,
    pub(crate) lifecycle: String,
    pub(crate) latency_micros: u64,
}

#[cfg(any(feature = "pg17", feature = "pg18"))]
mod supported {
    use std::cell::{Cell, RefCell};
    use std::ffi::{CStr, c_void};
    use std::mem::size_of;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    #[cfg(not(feature = "pg_test"))]
    use pgrx::bgworkers::BackgroundWorkerBuilder;
    use pgrx::bgworkers::{BackgroundWorker, SignalWakeFlags};
    use pgrx::prelude::*;

    use super::{AutomaticQuerySummary, ObservationToken, QueueSnapshot};
    use context_query::{ReadinessReason, StageDiagnostic, StageKind};

    // Bump the suffix whenever QueueHeader's process-shared layout changes so
    // a backend loading an upgraded extension never reinterprets an older
    // postmaster-lifetime mapping.
    const QUEUE_NAME: &CStr = c"pgcontext_query_telemetry_v8";
    #[cfg(feature = "pg_test")]
    const FAILURE_QUEUE_NAME: &CStr = c"pgcontext_query_telemetry_v8_failure";
    const QUEUE_CAPACITY: usize = 1024;
    const DATABASE_CAPACITY: usize = 64;
    const DRAIN_BATCH: usize = 64;
    const WORKER_WAIT: Duration = Duration::from_millis(10);
    const WORKER_IDLE_TIMEOUT_MICROS: i64 = 5_000_000;
    #[cfg(not(feature = "pg_test"))]
    const WORKER_START_TIMEOUT_MICROS: i64 = 30_000_000;
    const ORPHAN_TIMEOUT_MICROS: i64 = 60_000_000;
    const RETRY_DELAY_MICROS: i64 = 250_000;
    const LABEL_CAPACITY: usize = 64;
    const SLOT_EMPTY: u8 = 0;
    const SLOT_READY: u8 = 1;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct BoundedLabel {
        bytes: [u8; LABEL_CAPACITY],
        len: u8,
    }

    impl BoundedLabel {
        const EMPTY: Self = Self {
            bytes: [0; LABEL_CAPACITY],
            len: 0,
        };

        fn from_static(value: &'static str) -> Self {
            debug_assert!(value.len() <= LABEL_CAPACITY);
            let mut label = Self::EMPTY;
            let len = value.len().min(LABEL_CAPACITY);
            label.bytes[..len].copy_from_slice(&value.as_bytes()[..len]);
            label.len = u8::try_from(len).unwrap_or(u8::MAX);
            label
        }

        fn as_str(&self) -> &str {
            let len = usize::from(self.len).min(LABEL_CAPACITY);
            // Every producer accepts only compile-time ASCII labels from this
            // module or the typed executor; shared memory cannot be written by
            // SQL callers.
            std::str::from_utf8(&self.bytes[..len]).unwrap_or("unspecified")
        }
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct QueryEvent {
        database_oid: u32,
        extension_oid: u32,
        owner_oid: u32,
        collection_id: i64,
        created_at: i64,
        result_count: u64,
        visits: u64,
        filter_candidates: u64,
        candidates: u64,
        rechecks: u64,
        stages: u64,
        expansions: u64,
        latency_micros: u64,
        query_kind: BoundedLabel,
        strategy: BoundedLabel,
        completion: BoundedLabel,
        lifecycle: BoundedLabel,
        database_slot: u8,
        used_fallback: u8,
        saw_fusion: u8,
        saw_hnsw: u8,
        saw_quantized: u8,
    }

    impl QueryEvent {
        const EMPTY: Self = Self {
            database_oid: 0,
            extension_oid: 0,
            owner_oid: 0,
            collection_id: 0,
            created_at: 0,
            result_count: 0,
            visits: 0,
            filter_candidates: 0,
            candidates: 0,
            rechecks: 0,
            stages: 0,
            expansions: 0,
            latency_micros: 0,
            query_kind: BoundedLabel::EMPTY,
            strategy: BoundedLabel::EMPTY,
            completion: BoundedLabel::EMPTY,
            lifecycle: BoundedLabel::EMPTY,
            database_slot: 0,
            used_fallback: 0,
            saw_fusion: 0,
            saw_hnsw: 0,
            saw_quantized: 0,
        };
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct QueueEntry {
        sequence: u64,
        retry_at: i64,
        state: u8,
        event: QueryEvent,
    }

    const EMPTY_ENTRY: QueueEntry = QueueEntry {
        sequence: 0,
        retry_at: 0,
        state: SLOT_EMPTY,
        event: QueryEvent::EMPTY,
    };

    #[repr(C)]
    struct DatabaseSlot {
        database_oid: u32,
        extension_oid: u32,
        owner_oid: u32,
        worker_pid: i32,
        worker_proc_number: i32,
        worker_started_at: i64,
        worker_launch_nonce: u32,
        worker_launch_started_at: i64,
        enqueued: AtomicU64,
        persisted: AtomicU64,
        dropped_contention: AtomicU64,
        dropped_full: AtomicU64,
        dropped_orphaned: AtomicU64,
        worker_launch_failures: AtomicU64,
    }

    #[repr(C)]
    struct QueueHeader {
        lock: pg_sys::LWLock,
        tranche_id: i32,
        next_sequence: u64,
        database_slot_exhausted: AtomicU64,
        database_slots: [DatabaseSlot; DATABASE_CAPACITY],
        entries: [QueueEntry; QUEUE_CAPACITY],
    }

    #[derive(Clone, Copy)]
    struct ActiveObservation {
        token: ObservationToken,
        event: QueryEvent,
        started_at: i64,
    }

    #[derive(Clone, Copy)]
    struct PendingEntry {
        entry_index: usize,
        sequence: u64,
        event: QueryEvent,
    }

    thread_local! {
        static QUEUE_PTR: Cell<*mut QueueHeader> = const { Cell::new(std::ptr::null_mut()) };
        static TRANCHE_REGISTERED: Cell<bool> = const { Cell::new(false) };
        static ACTIVE: RefCell<Vec<ActiveObservation>> = const { RefCell::new(Vec::new()) };
        static NEXT_OBSERVATION: Cell<u64> = const { Cell::new(1) };
        static DATABASE_SLOT: Cell<Option<usize>> = const { Cell::new(None) };
        #[cfg(feature = "pg_test")]
        static TEST_EVENTS: RefCell<Vec<QueryEvent>> = const { RefCell::new(Vec::new()) };
        #[cfg(feature = "pg_test")]
        static TEST_PRODUCTION_COORDINATION: Cell<bool> = const { Cell::new(false) };
    }

    /// Initializes the shared header's process-shared lock and atomics.
    unsafe extern "C-unwind" fn queue_init_callback(ptr: *mut c_void) {
        let header = ptr.cast::<QueueHeader>();
        // SAFETY: PostgreSQL allocated exactly the requested header size and
        // serializes this one-time callback against all attachers.
        unsafe { header.write_bytes(0, 1) };
        // SAFETY: PostgreSQL is initialized when named DSM invokes callbacks.
        let tranche_id = unsafe { pg_sys::LWLockNewTrancheId() };
        // SAFETY: the callback exclusively owns the freshly zeroed header.
        unsafe {
            pg_sys::LWLockInitialize(core::ptr::addr_of_mut!((*header).lock), tranche_id);
            core::ptr::addr_of_mut!((*header).tranche_id).write(tranche_id);
            core::ptr::addr_of_mut!((*header).next_sequence).write(1);
            core::ptr::addr_of_mut!((*header).database_slot_exhausted).write(AtomicU64::new(0));
            for slot in &mut (*header).database_slots {
                core::ptr::addr_of_mut!(slot.enqueued).write(AtomicU64::new(0));
                core::ptr::addr_of_mut!(slot.persisted).write(AtomicU64::new(0));
                core::ptr::addr_of_mut!(slot.dropped_contention).write(AtomicU64::new(0));
                core::ptr::addr_of_mut!(slot.dropped_full).write(AtomicU64::new(0));
                core::ptr::addr_of_mut!(slot.dropped_orphaned).write(AtomicU64::new(0));
                core::ptr::addr_of_mut!(slot.worker_launch_failures).write(AtomicU64::new(0));
            }
        }
    }

    /// Attaches the server-wide named DSM queue for this backend.
    unsafe fn queue_ptr() -> *mut QueueHeader {
        QUEUE_PTR.with(|cached| {
            let existing = cached.get();
            if !existing.is_null() {
                return existing;
            }
            let mut found = false;
            // SAFETY: size and callback exactly match QueueHeader; PostgreSQL
            // retains the mapping for the backend lifetime.
            let ptr = unsafe {
                pg_sys::GetNamedDSMSegment(
                    QUEUE_NAME.as_ptr(),
                    size_of::<QueueHeader>(),
                    Some(queue_init_callback),
                    &mut found,
                )
            }
            .cast::<QueueHeader>();
            // SAFETY: initialization completed before GetNamedDSMSegment returned.
            let tranche_id = unsafe { (*ptr).tranche_id };
            TRANCHE_REGISTERED.with(|registered| {
                if !registered.get() {
                    // SAFETY: registration affects only this backend's tranche-name table.
                    unsafe {
                        pg_sys::LWLockRegisterTranche(
                            tranche_id,
                            c"pgcontext_query_telemetry".as_ptr(),
                        );
                    }
                    registered.set(true);
                }
            });
            cached.set(ptr);
            ptr
        })
    }

    fn try_queue_ptr() -> Option<*mut QueueHeader> {
        // Named-DSM allocation or attachment can raise when shared-memory
        // resources are exhausted or an old postmaster-lifetime layout has the
        // same name. PostgreSQL's registry function changes memory context and
        // holds a dshash lock across the fallible work, so a bare PG_TRY catch
        // is unsafe: rollback an internal subtransaction to release all error
        // resources before returning to retrieval.
        // SAFETY: this runs only inside a connected SQL transaction. The saved
        // context/owner are restored on both success and failure, following the
        // PostgreSQL procedural-language SPI error-recovery pattern.
        let (old_context, old_owner) = unsafe {
            let old_context = pg_sys::CurrentMemoryContext;
            let old_owner = pg_sys::CurrentResourceOwner;
            pg_sys::BeginInternalSubTransaction(std::ptr::null());
            pg_sys::MemoryContextSwitchTo(old_context);
            (old_context, old_owner)
        };
        let ptr = PgTryBuilder::new(|| {
            // SAFETY: queue_ptr owns the attach/initialize contract and caches
            // only a successfully returned postmaster-lifetime mapping.
            let ptr = unsafe { queue_ptr() };
            // SAFETY: commit the inner transaction, then restore the caller's
            // memory context and resource owner exactly once.
            unsafe {
                pg_sys::ReleaseCurrentSubTransaction();
                pg_sys::MemoryContextSwitchTo(old_context);
                pg_sys::CurrentResourceOwner = old_owner;
            }
            ptr
        })
        .catch_others(|_| {
            // SAFETY: discard the current error before aborting the inner
            // transaction, which releases the DSM-registry dshash/LWLocks and
            // resources acquired by the failed attachment. Restore the outer
            // context/owner before allowing retrieval to continue.
            unsafe {
                pg_sys::MemoryContextSwitchTo(old_context);
                pg_sys::FlushErrorState();
                pg_sys::RollbackAndReleaseCurrentSubTransaction();
                pg_sys::MemoryContextSwitchTo(old_context);
                pg_sys::CurrentResourceOwner = old_owner;
            }
            std::ptr::null_mut()
        })
        .execute();
        (!ptr.is_null()).then_some(ptr)
    }

    unsafe fn lock(header: *mut QueueHeader, mode: pg_sys::LWLockMode::Type) {
        // SAFETY: header is a live named-DSM mapping with an initialized lock.
        unsafe { pg_sys::LWLockAcquire(core::ptr::addr_of_mut!((*header).lock), mode) };
    }

    unsafe fn try_lock(header: *mut QueueHeader, mode: pg_sys::LWLockMode::Type) -> bool {
        // SAFETY: header is a live named-DSM mapping with an initialized lock.
        unsafe { pg_sys::LWLockConditionalAcquire(core::ptr::addr_of_mut!((*header).lock), mode) }
    }

    unsafe fn unlock(header: *mut QueueHeader) {
        // SAFETY: caller holds this header's LWLock.
        unsafe { pg_sys::LWLockRelease(core::ptr::addr_of_mut!((*header).lock)) };
    }

    fn current_database_oid() -> u32 {
        // SAFETY: query execution occurs only after database connection setup.
        unsafe { pg_sys::MyDatabaseId.to_u32() }
    }

    fn extension_identity() -> Option<(u32, u32)> {
        Spi::get_two::<pg_sys::Oid, pg_sys::Oid>(
            "SELECT oid, extowner FROM pg_catalog.pg_extension WHERE extname = 'pgcontext'",
        )
        .ok()
        .and_then(|(extension_oid, owner_oid)| extension_oid.zip(owner_oid))
        .map(|(extension_oid, owner_oid)| (extension_oid.to_u32(), owner_oid.to_u32()))
    }

    fn select_database_slot(
        slot_count: usize,
        mut identity: impl FnMut(usize) -> (u32, u32, u32),
        database_oid: u32,
        extension_oid: u32,
        owner_oid: u32,
    ) -> Option<usize> {
        (0..slot_count)
            .find(|&index| identity(index) == (database_oid, extension_oid, owner_oid))
            .or_else(|| (0..slot_count).find(|&index| identity(index).0 == database_oid))
            .or_else(|| (0..slot_count).find(|&index| identity(index).0 == 0))
    }

    fn ensure_database_slot() -> Option<(usize, u32, u32)> {
        let database_oid = current_database_oid();
        let (extension_oid, owner_oid) = extension_identity()?;
        // SAFETY: all header fields below are protected by its LWLock.
        let header = try_queue_ptr()?;
        // SAFETY: conditional acquisition never waits on telemetry work. A
        // contended queue drops this observation so retrieval can proceed.
        if !unsafe { try_lock(header, pg_sys::LWLockMode::LW_EXCLUSIVE) } {
            return None;
        }
        // SAFETY: lock excludes concurrent mutation.
        let slots = unsafe { &mut (*header).database_slots };
        if let Some(index) = DATABASE_SLOT.with(Cell::get)
            && slots[index].database_oid == database_oid
            && slots[index].extension_oid == extension_oid
            && slots[index].owner_oid == owner_oid
        {
            // SAFETY: paired with acquisition above.
            unsafe { unlock(header) };
            return Some((index, extension_oid, owner_oid));
        }
        // One database keeps one bounded slot across extension recreation and
        // owner changes. Entries from the replaced generation are explicitly
        // discarded below because that extension catalog no longer exists.
        let index = select_database_slot(
            slots.len(),
            |index| {
                let slot = &slots[index];
                (slot.database_oid, slot.extension_oid, slot.owner_oid)
            },
            database_oid,
            extension_oid,
            owner_oid,
        );
        if let Some(index) = index {
            let slot = &mut slots[index];
            if slot.database_oid == 0 {
                slot.database_oid = database_oid;
                slot.extension_oid = extension_oid;
                slot.owner_oid = owner_oid;
                slot.worker_pid = 0;
                slot.worker_proc_number = 0;
                slot.worker_started_at = 0;
                slot.worker_launch_started_at = 0;
            } else if slot.extension_oid != extension_oid || slot.owner_oid != owner_oid {
                let old_extension_oid = slot.extension_oid;
                let old_owner_oid = slot.owner_oid;
                // SAFETY: the same exclusive header lock protects queue-entry
                // state. A worker that already copied one of these entries will
                // fail its sequence/state check when it later acknowledges it.
                let entries = unsafe { &mut (*header).entries };
                let mut discarded = 0_u64;
                for entry in entries.iter_mut().filter(|entry| {
                    entry.state == SLOT_READY
                        && entry.event.database_oid == database_oid
                        && entry.event.extension_oid == old_extension_oid
                        && entry.event.owner_oid == old_owner_oid
                }) {
                    *entry = EMPTY_ENTRY;
                    discarded = discarded.saturating_add(1);
                }
                slot.dropped_orphaned
                    .fetch_add(discarded, Ordering::Relaxed);
                slot.extension_oid = extension_oid;
                slot.owner_oid = owner_oid;
                // Invalidate both a published worker and an in-flight launcher.
                // The launch nonce prevents either old process from publishing
                // or clearing the replacement generation afterward.
                slot.worker_pid = 0;
                slot.worker_proc_number = 0;
                slot.worker_started_at = 0;
                slot.worker_launch_started_at = 0;
            }
        }
        if index.is_none() {
            // SAFETY: the process-shared atomic was initialized with the header.
            unsafe { &(*header).database_slot_exhausted }.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: paired with acquisition above.
        unsafe { unlock(header) };
        DATABASE_SLOT.with(|cached| cached.set(index));
        index.map(|index| (index, extension_oid, owner_oid))
    }

    #[derive(Clone, Copy)]
    struct ProcessIdentity {
        pid: i32,
        proc_number: i32,
        started_at: i64,
        is_background_worker: bool,
    }

    fn process_identity_matches(
        expected: ProcessIdentity,
        actual: ProcessIdentity,
        require_background_worker: bool,
    ) -> bool {
        expected.pid == actual.pid
            && expected.proc_number == actual.proc_number
            && expected.started_at == actual.started_at
            && (!require_background_worker || actual.is_background_worker)
    }

    #[cfg(not(feature = "pg_test"))]
    fn process_identity_is_live(slot: &DatabaseSlot, require_background_worker: bool) -> bool {
        if slot.worker_pid == 0 || slot.worker_proc_number < 0 || slot.worker_started_at == 0 {
            return false;
        }
        let Ok(pid) = i32::try_from(slot.worker_pid.unsigned_abs()) else {
            return false;
        };
        // SAFETY: PostgreSQL accepts any positive pid and returns null when the
        // process is absent. A live PGPROC remains stable for this inspection.
        let process = unsafe { pg_sys::BackendPidGetProc(pid) };
        if process.is_null() {
            return false;
        }
        // SAFETY: a non-null PGPROC returned by PostgreSQL is readable while
        // the process remains registered. The stats entry adds its immutable
        // process-start timestamp so PID/PGPROC-slot reuse cannot match.
        let (actual_pid, actual_proc_number) =
            unsafe { ((*process).pid, (*process).vxid.procNumber) };
        // SAFETY: procNumber came from the live PGPROC and PostgreSQL returns
        // null when no status entry is associated with it.
        let status = unsafe { pg_sys::pgstat_get_beentry_by_proc_number(actual_proc_number) };
        if status.is_null() {
            return false;
        }
        // SAFETY: process start time and pid are immutable for this registered
        // backend; a mismatch is treated as not live.
        let (actual_started_at, actual_is_background_worker) = unsafe {
            (
                (*status).st_proc_start_timestamp,
                (*status).st_backendType == pg_sys::BackendType::B_BG_WORKER,
            )
        };
        process_identity_matches(
            ProcessIdentity {
                pid,
                proc_number: slot.worker_proc_number,
                started_at: slot.worker_started_at,
                is_background_worker: require_background_worker,
            },
            ProcessIdentity {
                pid: actual_pid,
                proc_number: actual_proc_number,
                started_at: actual_started_at,
                is_background_worker: actual_is_background_worker,
            },
            require_background_worker,
        )
    }

    fn current_process_identity() -> (i32, i64) {
        // SAFETY: these globals are initialized before SQL or background-worker
        // callbacks execute and remain immutable for the process lifetime.
        unsafe { (pg_sys::MyProcNumber, pg_sys::MyStartTimestamp) }
    }

    #[cfg(not(feature = "pg_test"))]
    fn next_launch_nonce(current: u32) -> u32 {
        current.wrapping_add(1).max(1)
    }

    #[cfg(not(feature = "pg_test"))]
    fn encode_worker_argument(database_slot: usize, launch_nonce: u32) -> Option<pg_sys::Datum> {
        let slot = u64::try_from(database_slot).ok()?;
        let packed = (u64::from(launch_nonce) << 8) | slot;
        Some(pg_sys::Datum::from(usize::try_from(packed).ok()?))
    }

    fn decode_worker_argument(argument: pg_sys::Datum) -> (usize, u32) {
        let packed = argument.value();
        (
            packed & 0xff,
            u32::try_from(packed >> 8).unwrap_or_default(),
        )
    }

    #[cfg(not(feature = "pg_test"))]
    fn ensure_worker(database_slot: usize) {
        // SAFETY: the queue mapping remains live for this backend.
        let header = unsafe { queue_ptr() };
        // SAFETY: a retrieval producer never waits for worker coordination.
        if !unsafe { try_lock(header, pg_sys::LWLockMode::LW_EXCLUSIVE) } {
            // SAFETY: fixed slot index and process-shared atomic initialization.
            unsafe { &(*header).database_slots[database_slot].dropped_contention }
                .fetch_add(1, Ordering::Relaxed);
            return;
        }
        // SAFETY: database_slot came from the fixed header array.
        let slot = unsafe { &mut (*header).database_slots[database_slot] };
        let launch_is_live = slot.worker_pid < 0
            && process_identity_is_live(slot, false)
            && now_monotonic_micros().saturating_sub(slot.worker_launch_started_at)
                < WORKER_START_TIMEOUT_MICROS;
        if (slot.worker_pid > 0 && process_identity_is_live(slot, true)) || launch_is_live {
            // SAFETY: paired with acquisition above.
            unsafe { unlock(header) };
            return;
        }
        // A negative pid marks a launcher until the worker replaces it with its
        // own positive pid. A dead launcher is safely reclaimed here.
        // SAFETY: MyProcPid is initialized in a connected backend.
        slot.worker_pid = -unsafe { pg_sys::MyProcPid };
        let (proc_number, started_at) = current_process_identity();
        slot.worker_proc_number = proc_number;
        slot.worker_started_at = started_at;
        slot.worker_launch_nonce = next_launch_nonce(slot.worker_launch_nonce);
        let launch_nonce = slot.worker_launch_nonce;
        slot.worker_launch_started_at = now_monotonic_micros();
        // SAFETY: paired with acquisition above.
        unsafe { unlock(header) };

        let loaded = BackgroundWorkerBuilder::new("pgContext query telemetry")
            .set_library("pgcontext")
            .set_function("pgcontext_query_telemetry_worker_main")
            .set_argument(encode_worker_argument(database_slot, launch_nonce))
            .enable_spi_access()
            .load_dynamic()
            .is_ok();
        if !loaded {
            // SAFETY: fixed valid index and initialized process-shared atomic.
            unsafe { &(*header).database_slots[database_slot].worker_launch_failures }
                .fetch_add(1, Ordering::Relaxed);
            // SAFETY: failure cleanup is also nonblocking for the producer.
            if unsafe { try_lock(header, pg_sys::LWLockMode::LW_EXCLUSIVE) } {
                // SAFETY: fixed valid index protected by the exclusive lock.
                let slot = unsafe { &mut (*header).database_slots[database_slot] };
                if slot.worker_launch_nonce == launch_nonce {
                    slot.worker_pid = 0;
                    slot.worker_proc_number = 0;
                    slot.worker_started_at = 0;
                    slot.worker_launch_started_at = 0;
                }
                // SAFETY: paired with conditional acquisition above.
                unsafe { unlock(header) };
            }
        }
    }

    fn now_monotonic_micros() -> i64 {
        // PostgreSQL's instrument clock is CLOCK_MONOTONIC on supported
        // platforms and is comparable across postmaster child processes.
        // SAFETY: the clock shim has no preconditions in backends or workers.
        unsafe { pg_sys::pg_clock_gettime_ns().ticks / 1_000 }
    }

    fn saturating_u64(value: usize) -> u64 {
        u64::try_from(value).unwrap_or(u64::MAX)
    }

    #[cfg(not(feature = "pg_test"))]
    fn enqueue(event: QueryEvent) {
        // SAFETY: begin() attached the queue before an event can become active.
        let header = unsafe { queue_ptr() };
        // SAFETY: event carries an index allocated from this fixed header.
        let database = unsafe { &(*header).database_slots[usize::from(event.database_slot)] };
        // SAFETY: conditional acquire is nonblocking and the lock is initialized.
        let acquired = unsafe {
            pg_sys::LWLockConditionalAcquire(
                core::ptr::addr_of_mut!((*header).lock),
                pg_sys::LWLockMode::LW_EXCLUSIVE,
            )
        };
        if !acquired {
            database.dropped_contention.fetch_add(1, Ordering::Relaxed);
            return;
        }
        // SAFETY: the exclusive lock protects entry and sequence mutation.
        let entries = unsafe { &mut (*header).entries };
        if let Some(entry) = entries.iter_mut().find(|entry| entry.state == SLOT_EMPTY) {
            // SAFETY: protected by the same lock as all sequence access.
            let sequence = unsafe { (*header).next_sequence };
            // SAFETY: exclusive lock protects the write.
            unsafe { (*header).next_sequence = sequence.wrapping_add(1).max(1) };
            *entry = QueueEntry {
                sequence,
                retry_at: 0,
                state: SLOT_READY,
                event,
            };
            database.enqueued.fetch_add(1, Ordering::Relaxed);
        } else {
            database.dropped_full.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: paired with conditional acquisition above.
        unsafe { unlock(header) };
    }

    fn strategy_from_partial(event: &QueryEvent) -> &'static str {
        if event.used_fallback != 0 {
            "dense_exact_fallback"
        } else if event.saw_fusion != 0 && event.saw_quantized != 0 {
            "composite_quantized_hnsw"
        } else if event.saw_fusion != 0 && event.saw_hnsw != 0 {
            "composite_hnsw"
        } else if event.saw_fusion != 0 {
            "composite_exact"
        } else {
            "unspecified"
        }
    }

    fn lifecycle_from_partial(event: &QueryEvent) -> &'static str {
        if event.used_fallback != 0 {
            "Fallback"
        } else if event.saw_hnsw != 0 {
            "Indexed"
        } else {
            "Unspecified"
        }
    }

    fn finish_active(mut active: ActiveObservation, sqlerrcode: Option<i32>) {
        let elapsed = now_monotonic_micros()
            .saturating_sub(active.started_at)
            .max(0);
        active.event.latency_micros = u64::try_from(elapsed).unwrap_or_default();
        active.event.created_at = now_monotonic_micros();
        if let Some(sqlerrcode) = sqlerrcode {
            active.event.completion = BoundedLabel::from_static(
                if sqlerrcode == PgSqlErrorCode::ERRCODE_QUERY_CANCELED as i32 {
                    "cancelled"
                } else if sqlerrcode == PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED as i32 {
                    "budget_exhausted"
                } else {
                    "error"
                },
            );
            active.event.lifecycle = BoundedLabel::from_static(
                if sqlerrcode == PgSqlErrorCode::ERRCODE_DATA_CORRUPTED as i32 {
                    "IndexCorrupt"
                } else if sqlerrcode
                    == PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE as i32
                {
                    "IndexNotReady"
                } else {
                    lifecycle_from_partial(&active.event)
                },
            );
            let partial_strategy = strategy_from_partial(&active.event);
            if partial_strategy != "unspecified" || active.event.strategy.as_str() == "unspecified"
            {
                active.event.strategy = BoundedLabel::from_static(partial_strategy);
            }
        }
        #[cfg(feature = "pg_test")]
        TEST_EVENTS.with(|events| events.borrow_mut().push(active.event));
        #[cfg(not(feature = "pg_test"))]
        {
            enqueue(active.event);
            // Launch after enqueue. The worker's final empty check and worker_pid
            // publication share the queue lock, so either it observes this event
            // or this producer observes pid 0 and starts a replacement.
            ensure_worker(usize::from(active.event.database_slot));
        }
    }

    pub(super) fn begin(
        collection_id: i64,
        query_kind: &'static str,
        used_fallback: bool,
    ) -> Option<ObservationToken> {
        let started_at = now_monotonic_micros();
        #[cfg(not(feature = "pg_test"))]
        let (database_slot, extension_oid, owner_oid) = ensure_database_slot()?;
        #[cfg(feature = "pg_test")]
        let (database_slot, extension_oid, owner_oid) =
            if TEST_PRODUCTION_COORDINATION.with(Cell::get) {
                ensure_database_slot()?
            } else {
                (0, 1, 1)
            };
        let event = QueryEvent {
            database_oid: current_database_oid(),
            extension_oid,
            owner_oid,
            collection_id,
            query_kind: BoundedLabel::from_static(query_kind),
            strategy: BoundedLabel::from_static(if used_fallback {
                "dense_exact_fallback"
            } else {
                "unspecified"
            }),
            completion: BoundedLabel::from_static("error"),
            lifecycle: BoundedLabel::from_static(if used_fallback {
                "Fallback"
            } else {
                "Unspecified"
            }),
            database_slot: u8::try_from(database_slot).unwrap_or_default(),
            used_fallback: u8::from(used_fallback),
            ..QueryEvent::EMPTY
        };
        let token = NEXT_OBSERVATION.with(|next| {
            let token = ObservationToken(next.get());
            next.set(next.get().wrapping_add(1).max(1));
            token
        });
        ACTIVE.with(|active| {
            active.borrow_mut().push(ActiveObservation {
                token,
                event,
                started_at,
            });
        });
        Some(token)
    }

    pub(super) fn record(diagnostic: &StageDiagnostic) {
        ACTIVE.with(|active| {
            let mut active = active.borrow_mut();
            let Some(active) = active.last_mut() else {
                return;
            };
            active.event.stages = active.event.stages.saturating_add(1);
            match diagnostic.stage() {
                StageKind::FilterCandidates => {
                    active.event.filter_candidates = active
                        .event
                        .filter_candidates
                        .saturating_add(saturating_u64(diagnostic.output_count()));
                }
                StageKind::Candidates => {
                    active.event.visits = active
                        .event
                        .visits
                        .saturating_add(saturating_u64(diagnostic.input_count()));
                    active.event.candidates = active
                        .event
                        .candidates
                        .saturating_add(saturating_u64(diagnostic.output_count()));
                    active.event.strategy = BoundedLabel::from_static(diagnostic.strategy());
                    active.event.saw_hnsw |= u8::from(
                        diagnostic.strategy().contains("hnsw")
                            || diagnostic.strategy().ends_with("_ann"),
                    );
                    active.event.saw_quantized |=
                        u8::from(diagnostic.strategy().contains("quantized"));
                }
                StageKind::SourceRecheck => {
                    active.event.rechecks = active
                        .event
                        .rechecks
                        .saturating_add(saturating_u64(diagnostic.input_count()));
                }
                StageKind::Fusion => active.event.saw_fusion = 1,
                StageKind::Readiness => {
                    if let Some(reason) = diagnostic.reason() {
                        active.event.lifecycle = BoundedLabel::from_static(match reason {
                            ReadinessReason::GenerationMissing => "ArtifactMissing",
                            ReadinessReason::ValidationFailed => "IndexCorrupt",
                            _ => "IndexNotReady",
                        });
                    }
                }
                StageKind::ScoreTransform | StageKind::Rerank => {}
            }
        });
    }

    pub(super) fn finish(token: ObservationToken, summary: AutomaticQuerySummary) {
        ACTIVE.with(|active| {
            let mut observations = active.borrow_mut();
            if observations.last().map(|active| active.token) != Some(token) {
                return;
            }
            let Some(mut active) = observations.pop() else {
                return;
            };
            active.event.result_count = saturating_u64(summary.result_count);
            active.event.visits = saturating_u64(summary.visits);
            active.event.filter_candidates = saturating_u64(summary.filter_candidates);
            active.event.candidates = saturating_u64(summary.candidates);
            active.event.rechecks = saturating_u64(summary.rechecks);
            active.event.stages = saturating_u64(summary.stages);
            active.event.expansions = saturating_u64(summary.expansions);
            active.event.completion = BoundedLabel::from_static(summary.completion);
            active.event.lifecycle = BoundedLabel::from_static(summary.lifecycle);
            active.event.strategy = BoundedLabel::from_static(summary.strategy);
            finish_active(active, None);
        });
    }

    pub(super) fn abort(token: ObservationToken, sqlerrcode: i32) {
        ACTIVE.with(|active| {
            let mut observations = active.borrow_mut();
            if observations.last().map(|active| active.token) == Some(token)
                && let Some(observation) = observations.pop()
            {
                finish_active(observation, Some(sqlerrcode));
            }
        });
    }

    fn pending_batch(
        header: *mut QueueHeader,
        database_oid: u32,
        extension_oid: u32,
        owner_oid: u32,
    ) -> Vec<PendingEntry> {
        let now = now_monotonic_micros();
        // SAFETY: caller receives owned copies while mutation is locked.
        unsafe { lock(header, pg_sys::LWLockMode::LW_SHARED) };
        // SAFETY: shared lock protects entries from concurrent writes.
        let batch = unsafe { &(*header).entries }
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                entry.state == SLOT_READY
                    && entry.event.database_oid == database_oid
                    && entry.event.extension_oid == extension_oid
                    && entry.event.owner_oid == owner_oid
                    && entry.retry_at <= now
            })
            .take(DRAIN_BATCH)
            .map(|(entry_index, entry)| PendingEntry {
                entry_index,
                sequence: entry.sequence,
                event: entry.event,
            })
            .collect();
        // SAFETY: paired with acquisition above.
        unsafe { unlock(header) };
        batch
    }

    fn acknowledge(header: *mut QueueHeader, pending: PendingEntry, persisted: bool) {
        // SAFETY: entry mutation is serialized by exclusive lock.
        unsafe { lock(header, pg_sys::LWLockMode::LW_EXCLUSIVE) };
        // SAFETY: pending index originated from the fixed array.
        let entry = unsafe { &mut (*header).entries[pending.entry_index] };
        if entry.state == SLOT_READY && entry.sequence == pending.sequence {
            *entry = EMPTY_ENTRY;
            // SAFETY: event's database slot was assigned from the fixed array.
            let database =
                unsafe { &(*header).database_slots[usize::from(pending.event.database_slot)] };
            if persisted {
                database.persisted.fetch_add(1, Ordering::Relaxed);
            } else {
                database.dropped_orphaned.fetch_add(1, Ordering::Relaxed);
            }
        }
        // SAFETY: paired with acquisition above.
        unsafe { unlock(header) };
    }

    fn defer(header: *mut QueueHeader, pending: PendingEntry) {
        // SAFETY: retry timestamp mutation is serialized by the exclusive lock.
        unsafe { lock(header, pg_sys::LWLockMode::LW_EXCLUSIVE) };
        // SAFETY: pending index originated from the fixed array.
        let entry = unsafe { &mut (*header).entries[pending.entry_index] };
        if entry.state == SLOT_READY && entry.sequence == pending.sequence {
            entry.retry_at = now_monotonic_micros().saturating_add(RETRY_DELAY_MICROS);
        }
        // SAFETY: paired with acquisition above.
        unsafe { unlock(header) };
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "microsecond telemetry is intentionally represented by PostgreSQL's double-precision millisecond API"
    )]
    fn persist(event: QueryEvent) -> bool {
        let latency_ms = event.latency_micros as f64 / 1_000.0;
        let result = Spi::get_one_with_args::<bool>(
            "WITH collection AS MATERIALIZED (
                 SELECT collection_id
                   FROM pgcontext._collections
                  WHERE collection_id = $1
             ), matching_extension AS MATERIALIZED (
                 SELECT oid
                   FROM pg_catalog.pg_extension
                  WHERE oid = $14
                    AND extname = 'pgcontext'
                    AND extowner = $15
             ), inserted AS (
                 INSERT INTO pgcontext._query_stats (
                     collection_id, cohort, query_kind, result_count,
                     candidate_count, rows_rechecked, rows_pruned,
                     recall_threshold, recall_achieved, latency_bucket,
                     lifecycle_state, latency_ms, strategy, visits,
                     filter_candidates, candidates, rechecks, stages,
                     expansions, completion
                 )
                 SELECT collection_id, 'automatic', $2, $3,
                        $6, $7, GREATEST($6 - $7, 0), NULL, NULL,
                        CASE
                            WHEN $11 < 1 THEN 'Lt1Ms'
                            WHEN $11 < 10 THEN 'Lt10Ms'
                            WHEN $11 < 100 THEN 'Lt100Ms'
                            WHEN $11 < 1000 THEN 'Lt1S'
                            ELSE 'Gte1S'
                        END,
                        $12, $11, $4, $5, $8, $6, $7, $9, $10, $13
                   FROM collection
                  CROSS JOIN matching_extension
                 RETURNING true
             )
             SELECT COALESCE((SELECT true FROM inserted), false)",
            &[
                event.collection_id.into(),
                event.query_kind.as_str().into(),
                i64::try_from(event.result_count).unwrap_or(i64::MAX).into(),
                event.strategy.as_str().into(),
                i64::try_from(event.visits).unwrap_or(i64::MAX).into(),
                i64::try_from(event.candidates).unwrap_or(i64::MAX).into(),
                i64::try_from(event.rechecks).unwrap_or(i64::MAX).into(),
                i64::try_from(event.filter_candidates)
                    .unwrap_or(i64::MAX)
                    .into(),
                i64::try_from(event.stages).unwrap_or(i64::MAX).into(),
                i64::try_from(event.expansions).unwrap_or(i64::MAX).into(),
                latency_ms.into(),
                event.lifecycle.as_str().into(),
                event.completion.as_str().into(),
                pg_sys::Oid::from_u32(event.extension_oid).into(),
                pg_sys::Oid::from_u32(event.owner_oid).into(),
            ],
        );
        result.ok().flatten().unwrap_or(false)
    }

    fn worker_exit(header: *mut QueueHeader, database_slot: usize, launch_nonce: u32) {
        // SAFETY: worker slot mutation is serialized by the header lock.
        unsafe { lock(header, pg_sys::LWLockMode::LW_EXCLUSIVE) };
        // SAFETY: database_slot is the worker's validated startup argument.
        let slot = unsafe { &mut (*header).database_slots[database_slot] };
        // SAFETY: initialized in this background worker.
        if slot.worker_launch_nonce == launch_nonce
            && slot.worker_pid == unsafe { pg_sys::MyProcPid }
        {
            slot.worker_pid = 0;
            slot.worker_proc_number = 0;
            slot.worker_started_at = 0;
            slot.worker_launch_started_at = 0;
        }
        // SAFETY: paired with acquisition above.
        unsafe { unlock(header) };
    }

    unsafe extern "C-unwind" fn worker_before_shmem_exit(_code: i32, argument: pg_sys::Datum) {
        let (database_slot, launch_nonce) = decode_worker_argument(argument);
        if database_slot >= DATABASE_CAPACITY || launch_nonce == 0 {
            return;
        }
        // SAFETY: the worker attached and cached this mapping before registering
        // the exit callback; it remains valid through before_shmem_exit.
        let header = unsafe { queue_ptr() };
        worker_exit(header, database_slot, launch_nonce);
    }

    #[pg_guard]
    #[unsafe(no_mangle)]
    pub extern "C-unwind" fn pgcontext_query_telemetry_worker_main(argument: pg_sys::Datum) {
        let (database_slot, launch_nonce) = decode_worker_argument(argument);
        if database_slot >= DATABASE_CAPACITY || launch_nonce == 0 {
            return;
        }
        // SAFETY: named DSM is available to dynamic workers and lives for this process.
        let header = unsafe { queue_ptr() };
        // SAFETY: argument contains only the bounded slot and launch nonce. The
        // callback clears ownership on normal return and PostgreSQL ERROR exits.
        unsafe { pg_sys::before_shmem_exit(Some(worker_before_shmem_exit), argument) };
        // SAFETY: worker startup reads its slot under the shared lock.
        unsafe { lock(header, pg_sys::LWLockMode::LW_SHARED) };
        // SAFETY: range checked above and protected by the lock.
        let (database_oid, extension_oid, owner_oid, nonce_matches) = unsafe {
            let slot = &(*header).database_slots[database_slot];
            (
                slot.database_oid,
                slot.extension_oid,
                slot.owner_oid,
                slot.worker_launch_nonce == launch_nonce,
            )
        };
        // SAFETY: paired with acquisition above.
        unsafe { unlock(header) };
        if database_oid == 0 || extension_oid == 0 || owner_oid == 0 || !nonce_matches {
            return;
        }

        BackgroundWorker::attach_signal_handlers(
            SignalWakeFlags::SIGHUP | SignalWakeFlags::SIGTERM,
        );
        BackgroundWorker::connect_worker_to_spi_by_oid(
            Some(pg_sys::Oid::from_u32(database_oid)),
            Some(pg_sys::Oid::from_u32(owner_oid)),
        );
        // SAFETY: the named DSM header remains mapped in this worker. The lock
        // protects publication against an extension recreation that happened
        // after this worker copied its startup identity.
        unsafe { lock(header, pg_sys::LWLockMode::LW_EXCLUSIVE) };
        // SAFETY: fixed valid index and the exclusive lock protects identity
        // and pid publication.
        let slot = unsafe { &mut (*header).database_slots[database_slot] };
        let generation_matches = slot.database_oid == database_oid
            && slot.extension_oid == extension_oid
            && slot.owner_oid == owner_oid
            && slot.worker_launch_nonce == launch_nonce;
        // A positive pid belongs to a worker that won publication while this
        // worker was starting. Do not displace it, even for the same generation.
        let publication_available = slot.worker_pid <= 0;
        if generation_matches && publication_available {
            // SAFETY: MyProcPid is initialized in this background worker.
            let current_pid = unsafe { pg_sys::MyProcPid };
            slot.worker_pid = current_pid;
            let (proc_number, started_at) = current_process_identity();
            slot.worker_proc_number = proc_number;
            slot.worker_started_at = started_at;
            slot.worker_launch_started_at = 0;
        }
        // SAFETY: paired with acquisition above.
        unsafe { unlock(header) };
        if !generation_matches || !publication_available {
            return;
        }

        let mut idle_since = now_monotonic_micros();
        while BackgroundWorker::wait_latch(Some(WORKER_WAIT)) {
            let batch = pending_batch(header, database_oid, extension_oid, owner_oid);
            if batch.is_empty() {
                if now_monotonic_micros().saturating_sub(idle_since) >= WORKER_IDLE_TIMEOUT_MICROS {
                    // Serialize the final empty/idle check with producer worker
                    // discovery. A producer enqueues before examining worker_pid,
                    // so this worker either sees pending work or the producer
                    // observes pid 0 and launches the replacement.
                    // SAFETY: header is live and all plain fields use this lock.
                    unsafe { lock(header, pg_sys::LWLockMode::LW_EXCLUSIVE) };
                    // SAFETY: lock protects entry state and worker pid.
                    let still_pending = unsafe { &(*header).entries }.iter().any(|entry| {
                        entry.state == SLOT_READY
                            && entry.event.database_oid == database_oid
                            && entry.event.extension_oid == extension_oid
                            && entry.event.owner_oid == owner_oid
                    });
                    if !still_pending {
                        // An older generation may share this slot while it
                        // finishes its idle loop. It must never unpublish the
                        // replacement generation's worker.
                        // SAFETY: MyProcPid is initialized in this worker and
                        // the exclusive lock protects pid publication.
                        let slot = unsafe { &mut (*header).database_slots[database_slot] };
                        if slot.worker_launch_nonce == launch_nonce
                            && slot.worker_pid == unsafe { pg_sys::MyProcPid }
                        {
                            slot.worker_pid = 0;
                            slot.worker_proc_number = 0;
                            slot.worker_started_at = 0;
                            slot.worker_launch_started_at = 0;
                        }
                        // SAFETY: paired with acquisition above.
                        unsafe { unlock(header) };
                        return;
                    }
                    // SAFETY: paired with acquisition above.
                    unsafe { unlock(header) };
                    idle_since = now_monotonic_micros();
                }
                continue;
            }
            idle_since = now_monotonic_micros();
            for pending in batch {
                let generation_matches = BackgroundWorker::transaction(|| {
                    extension_identity()
                        .is_some_and(|identity| identity == (extension_oid, owner_oid))
                });
                if !generation_matches {
                    acknowledge(header, pending, false);
                    continue;
                }
                let inserted = BackgroundWorker::transaction(|| persist(pending.event));
                if inserted {
                    acknowledge(header, pending, true);
                } else if now_monotonic_micros().saturating_sub(pending.event.created_at)
                    >= ORPHAN_TIMEOUT_MICROS
                {
                    acknowledge(header, pending, false);
                } else {
                    defer(header, pending);
                }
            }
        }
        worker_exit(header, database_slot, launch_nonce);
    }

    pub(super) fn snapshot() -> QueueSnapshot {
        #[cfg(feature = "pg_test")]
        return TEST_EVENTS.with(|events| {
            let pending = events.borrow().len() as u64;
            QueueSnapshot {
                enqueued: pending,
                pending,
                ..QueueSnapshot::default()
            }
        });
        #[cfg(not(feature = "pg_test"))]
        {
            let database_oid = current_database_oid();
            let Some((extension_oid, owner_oid)) = extension_identity() else {
                return QueueSnapshot::default();
            };
            let Some(header) = try_queue_ptr() else {
                return QueueSnapshot::default();
            };
            // SAFETY: protect database slot and pending-entry reads.
            unsafe { lock(header, pg_sys::LWLockMode::LW_SHARED) };
            // SAFETY: shared lock protects plain slot identity/pid fields.
            let slot = unsafe { &(*header).database_slots }.iter().find(|slot| {
                slot.database_oid == database_oid
                    && slot.extension_oid == extension_oid
                    && slot.owner_oid == owner_oid
            });
            // SAFETY: shared lock protects entry state and event identity.
            let pending = unsafe { &(*header).entries }
                .iter()
                .filter(|entry| {
                    entry.state == SLOT_READY
                        && entry.event.database_oid == database_oid
                        && entry.event.extension_oid == extension_oid
                        && entry.event.owner_oid == owner_oid
                })
                .count() as u64;
            // SAFETY: shared atomic initialized before any backend attaches.
            let database_slot_exhausted =
                unsafe { &(*header).database_slot_exhausted }.load(Ordering::Relaxed);
            let snapshot = slot.map_or_else(
                || QueueSnapshot {
                    database_slot_exhausted,
                    ..QueueSnapshot::default()
                },
                |slot| QueueSnapshot {
                    enqueued: slot.enqueued.load(Ordering::Relaxed),
                    persisted: slot.persisted.load(Ordering::Relaxed),
                    dropped_contention: slot.dropped_contention.load(Ordering::Relaxed),
                    dropped_full: slot.dropped_full.load(Ordering::Relaxed),
                    dropped_orphaned: slot.dropped_orphaned.load(Ordering::Relaxed),
                    database_slot_exhausted,
                    worker_launch_failures: slot.worker_launch_failures.load(Ordering::Relaxed),
                    pending,
                    worker_pid: (slot.worker_pid > 0).then_some(slot.worker_pid),
                },
            );
            // SAFETY: paired with acquisition above.
            unsafe { unlock(header) };
            snapshot
        }
    }

    #[cfg(feature = "pg_test")]
    pub(super) fn test_events(collection_id: i64) -> Vec<super::TestEventSnapshot> {
        TEST_EVENTS.with(|events| {
            events
                .borrow()
                .iter()
                .filter(|event| event.collection_id == collection_id)
                .map(|event| super::TestEventSnapshot {
                    collection_id: event.collection_id,
                    query_kind: event.query_kind.as_str().to_owned(),
                    strategy: event.strategy.as_str().to_owned(),
                    result_count: event.result_count,
                    visits: event.visits,
                    filter_candidates: event.filter_candidates,
                    candidates: event.candidates,
                    rechecks: event.rechecks,
                    stages: event.stages,
                    expansions: event.expansions,
                    completion: event.completion.as_str().to_owned(),
                    lifecycle: event.lifecycle.as_str().to_owned(),
                    latency_micros: event.latency_micros,
                })
                .collect()
        })
    }

    #[cfg(feature = "pg_test")]
    pub(super) fn test_database_slot_generations_reuse_one_slot() -> bool {
        let mut identities = [(0_u32, 0_u32, 0_u32); DATABASE_CAPACITY];
        let mut selected = None;
        for generation in 1..=(DATABASE_CAPACITY as u32 + 16) {
            let Some(index) = select_database_slot(
                identities.len(),
                |index| identities[index],
                42,
                1_000 + generation,
                10 + generation,
            ) else {
                return false;
            };
            identities[index] = (42, 1_000 + generation, 10 + generation);
            if selected
                .replace(index)
                .is_some_and(|previous| previous != index)
            {
                return false;
            }
        }
        selected == Some(0) && identities.iter().skip(1).all(|identity| identity.0 == 0)
    }

    #[cfg(feature = "pg_test")]
    pub(super) fn test_with_producer_lock_contention<T>(callback: impl FnOnce() -> T) -> T {
        // SAFETY: this backend owns the test queue mapping for the duration of
        // the callback; the production acquisition path must fail conditionally.
        let header = unsafe { queue_ptr() };
        // SAFETY: the test deliberately owns the lock while invoking retrieval.
        unsafe { lock(header, pg_sys::LWLockMode::LW_EXCLUSIVE) };
        TEST_PRODUCTION_COORDINATION.with(|enabled| enabled.set(true));
        let result = callback();
        TEST_PRODUCTION_COORDINATION.with(|enabled| enabled.set(false));
        // SAFETY: paired with the deliberate test acquisition above.
        unsafe { unlock(header) };
        result
    }

    #[cfg(feature = "pg_test")]
    pub(super) fn test_pid_reuse_is_rejected() -> bool {
        let expected = ProcessIdentity {
            pid: 4242,
            proc_number: 7,
            started_at: 100,
            is_background_worker: true,
        };
        !process_identity_matches(
            expected,
            ProcessIdentity {
                started_at: 101,
                ..expected
            },
            false,
        ) && !process_identity_matches(
            expected,
            ProcessIdentity {
                proc_number: 8,
                ..expected
            },
            false,
        ) && !process_identity_matches(
            expected,
            ProcessIdentity {
                is_background_worker: false,
                ..expected
            },
            true,
        )
    }

    #[cfg(feature = "pg_test")]
    pub(super) fn test_failed_first_attach_recovers() -> bool {
        let mut found = false;
        // SAFETY: install a deliberately incompatible mapping under the test
        // queue name. pg_test observations are backend-local and never use the
        // production queue, so this cannot affect another test path.
        unsafe {
            pg_sys::GetNamedDSMSegment(FAILURE_QUEUE_NAME.as_ptr(), 1, None, &mut found);
        }
        // Both calls force the size-mismatch ERROR path. The second proves the
        // first recovery released the registry lock instead of deadlocking.
        try_incompatible_queue_ptr().is_none()
            && try_incompatible_queue_ptr().is_none()
            && Spi::get_one::<i32>("SELECT 1").ok().flatten() == Some(1)
    }

    #[cfg(feature = "pg_test")]
    fn try_incompatible_queue_ptr() -> Option<*mut QueueHeader> {
        // This mirrors try_queue_ptr's production error boundary while using a
        // dedicated name so the deliberately bad segment cannot poison later
        // tests that exercise the real coordination path.
        // SAFETY: pgrx tests execute inside a connected SQL transaction.
        let (old_context, old_owner) = unsafe {
            let old_context = pg_sys::CurrentMemoryContext;
            let old_owner = pg_sys::CurrentResourceOwner;
            pg_sys::BeginInternalSubTransaction(std::ptr::null());
            pg_sys::MemoryContextSwitchTo(old_context);
            (old_context, old_owner)
        };
        let ptr = PgTryBuilder::new(|| {
            let mut found = false;
            // SAFETY: the existing one-byte segment deliberately violates this
            // requested layout and raises before a pointer can be used.
            let ptr = unsafe {
                pg_sys::GetNamedDSMSegment(
                    FAILURE_QUEUE_NAME.as_ptr(),
                    size_of::<QueueHeader>(),
                    Some(queue_init_callback),
                    &mut found,
                )
            }
            .cast::<QueueHeader>();
            // SAFETY: success would own a valid inner transaction.
            unsafe {
                pg_sys::ReleaseCurrentSubTransaction();
                pg_sys::MemoryContextSwitchTo(old_context);
                pg_sys::CurrentResourceOwner = old_owner;
            }
            ptr
        })
        .catch_others(|_| {
            // SAFETY: clear and roll back the deliberate attachment error.
            unsafe {
                pg_sys::MemoryContextSwitchTo(old_context);
                pg_sys::FlushErrorState();
                pg_sys::RollbackAndReleaseCurrentSubTransaction();
                pg_sys::MemoryContextSwitchTo(old_context);
                pg_sys::CurrentResourceOwner = old_owner;
            }
            std::ptr::null_mut()
        })
        .execute();
        (!ptr.is_null()).then_some(ptr)
    }
}

/// Starts one active observation after collection authorization succeeds.
pub(crate) fn begin(
    collection_id: i64,
    query_kind: &'static str,
    used_fallback: bool,
) -> Option<ObservationToken> {
    if !crate::settings::query_telemetry_enabled() {
        return None;
    }
    #[cfg(any(feature = "pg17", feature = "pg18"))]
    return supported::begin(collection_id, query_kind, used_fallback);
    #[cfg(not(any(feature = "pg17", feature = "pg18")))]
    let _ = (collection_id, query_kind, used_fallback);
    #[cfg(not(any(feature = "pg17", feature = "pg18")))]
    None
}

/// Adds one bounded executor diagnostic to the active observation.
pub(crate) fn record(diagnostic: &StageDiagnostic) {
    #[cfg(any(feature = "pg17", feature = "pg18"))]
    supported::record(diagnostic);
    #[cfg(not(any(feature = "pg17", feature = "pg18")))]
    let _ = diagnostic;
}

/// Completes and enqueues the active observation without performing SQL writes.
pub(crate) fn finish(token: ObservationToken, summary: AutomaticQuerySummary) {
    #[cfg(any(feature = "pg17", feature = "pg18"))]
    supported::finish(token, summary);
    #[cfg(not(any(feature = "pg17", feature = "pg18")))]
    let _ = (token, summary);
}

/// Completes the innermost observation from a caught PostgreSQL terminal error.
pub(crate) fn abort(token: ObservationToken, sqlerrcode: i32) {
    #[cfg(any(feature = "pg17", feature = "pg18"))]
    supported::abort(token, sqlerrcode);
    #[cfg(not(any(feature = "pg17", feature = "pg18")))]
    let _ = (token, sqlerrcode);
}

/// Returns queue health for the current database only.
pub(crate) fn snapshot() -> QueueSnapshot {
    #[cfg(any(feature = "pg17", feature = "pg18"))]
    return supported::snapshot();
    #[cfg(not(any(feature = "pg17", feature = "pg18")))]
    QueueSnapshot::default()
}

/// Returns backend-local captured events in pgrx test builds.
#[cfg(feature = "pg_test")]
pub(crate) fn test_events(collection_id: i64) -> Vec<TestEventSnapshot> {
    #[cfg(any(feature = "pg17", feature = "pg18"))]
    return supported::test_events(collection_id);
    #[cfg(not(any(feature = "pg17", feature = "pg18")))]
    let _ = collection_id;
    #[cfg(not(any(feature = "pg17", feature = "pg18")))]
    Vec::new()
}

/// Proves one database reuses its bounded slot across extension generations.
#[cfg(feature = "pg_test")]
pub(crate) fn test_database_slot_generations_reuse_one_slot() -> bool {
    #[cfg(any(feature = "pg17", feature = "pg18"))]
    return supported::test_database_slot_generations_reuse_one_slot();
    #[cfg(not(any(feature = "pg17", feature = "pg18")))]
    true
}

/// Holds the real queue lock while a pgrx test executes one retrieval callback.
#[cfg(feature = "pg_test")]
pub(crate) fn test_with_producer_lock_contention<T>(callback: impl FnOnce() -> T) -> T {
    #[cfg(any(feature = "pg17", feature = "pg18"))]
    return supported::test_with_producer_lock_contention(callback);
    #[cfg(not(any(feature = "pg17", feature = "pg18")))]
    callback()
}

/// Proves reused PID values cannot match a different process incarnation.
#[cfg(feature = "pg_test")]
pub(crate) fn test_pid_reuse_is_rejected() -> bool {
    #[cfg(any(feature = "pg17", feature = "pg18"))]
    return supported::test_pid_reuse_is_rejected();
    #[cfg(not(any(feature = "pg17", feature = "pg18")))]
    true
}

/// Forces a named-DSM first-attach error and proves recovery leaves PostgreSQL usable.
#[cfg(feature = "pg_test")]
pub(crate) fn test_failed_first_attach_recovers() -> bool {
    #[cfg(any(feature = "pg17", feature = "pg18"))]
    return supported::test_failed_first_attach_recovers();
    #[cfg(not(any(feature = "pg17", feature = "pg18")))]
    true
}
