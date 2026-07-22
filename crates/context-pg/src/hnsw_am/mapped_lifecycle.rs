// Transaction-safe lifecycle hooks for mapped HNSW index directories.

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct PendingMappedIndexDrop {
    database_oid: u32,
    index_oid: u32,
    transaction_id: pg_sys::TransactionId,
    subtransaction_id: pg_sys::SubTransactionId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MappedIndexDropMarker {
    database_oid: u32,
    index_oid: u32,
    transaction_id: pg_sys::TransactionId,
}

const MAPPED_DROP_MARKER_PREFIX: &str = ".pending_drop_";
const MAPPED_DROP_MARKER_SUFFIX: &str = ".pgctxdrop";
const MAPPED_DROP_MARKER_DIRECTORY: &str = ".pending_drops";
const MAPPED_DROP_RETRY_DIRECTORY: &str = ".pending_drop_retries";
const MAPPED_DROP_TEMP_DIRECTORY: &str = ".pending_drop_temps";
const MAPPED_DROP_CURSOR_NAME: &str = ".pending_drop_bucket_cursor";
const MAPPED_DROP_CURSOR_LOCK_NAME: &str = ".pending_drop_bucket_cursor.lock";
const MAPPED_DROP_BUCKET_COUNT: usize = 16;
const MAPPED_DROP_RECONCILE_BUDGET: usize = 16;

thread_local! {
    static PENDING_MAPPED_INDEX_DROPS: RefCell<BTreeSet<PendingMappedIndexDrop>> =
        const { RefCell::new(BTreeSet::new()) };
}

static mut PREVIOUS_OBJECT_ACCESS_HOOK: pg_sys::object_access_hook_type = None;
static mut MAPPED_LIFECYCLE_HOOKS_INSTALLED: bool = false;
static MAPPED_SQL_DROP_FINFO: pg_sys::Pg_finfo_record =
    pg_sys::Pg_finfo_record { api_version: 1 };

pgrx::extension_sql!(
    r#"
CREATE FUNCTION pgcontext._mapped_hnsw_sql_drop()
RETURNS event_trigger
AS 'MODULE_PATHNAME', 'pgcontext_hnsw_mapped_sql_drop'
LANGUAGE C;

CREATE EVENT TRIGGER pgcontext_mapped_hnsw_sql_drop
    ON sql_drop
    EXECUTE FUNCTION pgcontext._mapped_hnsw_sql_drop();
"#,
    name = "mapped_hnsw_lifecycle_event_trigger",
    requires = [pgcontext]
);

pub(crate) fn init_mapped_graph_lifecycle_hooks() {
    // SAFETY: `_PG_init` runs once while the backend is single-threaded. The
    // installed callbacks retain only copied OIDs/subtransaction IDs and chain
    // the previously installed object-access hook.
    unsafe {
        if MAPPED_LIFECYCLE_HOOKS_INSTALLED {
            return;
        }
        PREVIOUS_OBJECT_ACCESS_HOOK = pg_sys::object_access_hook;
        pg_sys::object_access_hook = Some(mapped_graph_object_access_hook);
        pg_sys::RegisterXactCallback(Some(mapped_graph_xact_callback), ptr::null_mut());
        pg_sys::RegisterSubXactCallback(Some(mapped_graph_subxact_callback), ptr::null_mut());
        MAPPED_LIFECYCLE_HOOKS_INSTALLED = true;
    }
}

fn queue_mapped_index_drop(index_oid: pg_sys::Oid) -> std::io::Result<()> {
    // SAFETY: SQL-drop and object-access callbacks run inside an active
    // transaction after PostgreSQL initialized database/subtransaction state.
    let database_oid = unsafe { pg_sys::MyDatabaseId.to_u32() };
    let index_oid = index_oid.to_u32();
    let Some(directory) = hnsw_mapped_index_directory(database_oid, index_oid) else {
        return Ok(());
    };
    if !directory.is_dir() {
        return Ok(());
    }
    // Assigning the top-level XID lets a later backend distinguish a prepared
    // drop from a committed or aborted one after this backend exits.
    let pending = unsafe {
        PendingMappedIndexDrop {
            database_oid,
            index_oid,
            transaction_id: pg_sys::GetTopTransactionId(),
            subtransaction_id: pg_sys::GetCurrentSubTransactionId(),
        }
    };
    PENDING_MAPPED_INDEX_DROPS.with(|drops| {
        drops.borrow_mut().insert(pending);
    });
    persist_mapped_drop_marker(pending.into())
}

impl From<PendingMappedIndexDrop> for MappedIndexDropMarker {
    fn from(pending: PendingMappedIndexDrop) -> Self {
        Self {
            database_oid: pending.database_oid,
            index_oid: pending.index_oid,
            transaction_id: pending.transaction_id,
        }
    }
}

/// Returns PostgreSQL V1 function metadata for the mapped lifecycle event
/// trigger.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn pg_finfo_pgcontext_hnsw_mapped_sql_drop()
-> *const pg_sys::Pg_finfo_record {
    &MAPPED_SQL_DROP_FINFO
}

/// Queues dropped relation OIDs for transaction-commit reclamation.
///
/// # Safety
///
/// PostgreSQL must call this symbol as the `sql_drop` event-trigger function
/// through a valid V1 `FunctionCallInfo`.
#[pg_guard]
#[allow(unused_qualifications)]
#[unsafe(no_mangle)]
// SAFETY: PostgreSQL invokes this symbol only through the V1 event-trigger
// declaration installed below, with call-info live for the guarded call.
pub unsafe extern "C-unwind" fn pgcontext_hnsw_mapped_sql_drop(
    fcinfo: pg_sys::FunctionCallInfo,
) -> pg_sys::Datum {
    // SAFETY: The event-trigger manager owns call-info for this guarded call.
    let scope = unsafe { PgCallbackScope::new() };
    // SAFETY: PostgreSQL retains call-info until this function returns.
    let _fcinfo = unsafe { scope.borrow(fcinfo, "event-trigger FunctionCallInfo") };
    self::mapped_hnsw_sql_drop_safe()
}

fn mapped_hnsw_sql_drop_safe() -> pg_sys::Datum {
    let dropped_oids = Spi::connect(|client| {
        let rows = client.select(
            "SELECT objid::oid
               FROM pg_catalog.pg_event_trigger_dropped_objects()
              WHERE classid = 'pg_catalog.pg_class'::pg_catalog.regclass::oid
                AND objsubid = 0",
            None,
            &[],
        )?;
        rows.into_iter()
            .map(|row| row.get::<pg_sys::Oid>(1))
            .collect::<Result<Vec<_>, _>>()
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to queue mapped HNSW SQL-drop cleanup: {error}"),
        )
    });
    for object_oid in dropped_oids.into_iter().flatten() {
        if let Err(error) = queue_mapped_index_drop(object_oid) {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_IO_ERROR,
                format!("failed to durably queue mapped HNSW cleanup: {error}"),
            );
        }
    }
    pg_sys::Datum::from(0)
}

#[pg_guard]
unsafe extern "C-unwind" fn mapped_graph_object_access_hook(
    access: pg_sys::ObjectAccessType::Type,
    class_id: pg_sys::Oid,
    object_id: pg_sys::Oid,
    sub_id: i32,
    argument: *mut c_void,
) {
    // SAFETY: Hook chaining preserves PostgreSQL's callback arguments exactly.
    if let Some(previous) = unsafe { PREVIOUS_OBJECT_ACCESS_HOOK } {
        unsafe { previous(access, class_id, object_id, sub_id, argument) };
    }
    if access != pg_sys::ObjectAccessType::OAT_DROP
        || class_id != pg_sys::RelationRelationId
        || sub_id != 0
    {
        return;
    }

    // Queue every dropped pg_class OID that owns a mapped directory. The SQL
    // event trigger repeats this operation and fails the DDL closed if durable
    // marker publication fails.
    if let Err(error) = queue_mapped_index_drop(object_id) {
        pgrx::debug1!("pgcontext mapped HNSW drop-marker write failed: {error}");
    }
}

#[pg_guard]
unsafe extern "C-unwind" fn mapped_graph_xact_callback(
    event: pg_sys::XactEvent::Type,
    _argument: *mut c_void,
) {
    match event {
        pg_sys::XactEvent::XACT_EVENT_COMMIT | pg_sys::XactEvent::XACT_EVENT_PARALLEL_COMMIT => {
            let pending = PENDING_MAPPED_INDEX_DROPS.with(|drops| {
                std::mem::take(&mut *drops.borrow_mut())
            });
            for drop in pending {
                cleanup_committed_mapped_index_drop(drop.into());
            }
        }
        pg_sys::XactEvent::XACT_EVENT_ABORT
        | pg_sys::XactEvent::XACT_EVENT_PARALLEL_ABORT => {
            let pending = PENDING_MAPPED_INDEX_DROPS.with(|drops| {
                std::mem::take(&mut *drops.borrow_mut())
            });
            for drop in pending {
                remove_mapped_drop_marker(drop.into());
            }
        }
        pg_sys::XactEvent::XACT_EVENT_PREPARE => {
            // The durable markers outlive this backend and remain pending while
            // PostgreSQL reports the prepared XID as in progress.
            PENDING_MAPPED_INDEX_DROPS.with(|drops| drops.borrow_mut().clear());
        }
        _ => {}
    }
}

#[pg_guard]
unsafe extern "C-unwind" fn mapped_graph_subxact_callback(
    event: pg_sys::SubXactEvent::Type,
    subtransaction_id: pg_sys::SubTransactionId,
    parent_id: pg_sys::SubTransactionId,
    _argument: *mut c_void,
) {
    match event {
        pg_sys::SubXactEvent::SUBXACT_EVENT_ABORT_SUB => {
            let removed = PENDING_MAPPED_INDEX_DROPS.with(|drops| {
                let mut drops = drops.borrow_mut();
                let removed = drops
                    .iter()
                    .filter(|drop| drop.subtransaction_id == subtransaction_id)
                    .copied()
                    .collect::<Vec<_>>();
                drops.retain(|drop| drop.subtransaction_id != subtransaction_id);
                removed
            });
            for drop in removed {
                let marker = MappedIndexDropMarker::from(drop);
                let still_pending = PENDING_MAPPED_INDEX_DROPS.with(|drops| {
                    drops.borrow().iter().any(|candidate| {
                        MappedIndexDropMarker::from(*candidate) == marker
                    })
                });
                if !still_pending {
                    remove_mapped_drop_marker(marker);
                }
            }
        }
        pg_sys::SubXactEvent::SUBXACT_EVENT_COMMIT_SUB => {
            PENDING_MAPPED_INDEX_DROPS.with(|drops| {
                let mut drops = drops.borrow_mut();
                let promoted = drops
                    .iter()
                    .filter(|drop| drop.subtransaction_id == subtransaction_id)
                    .copied()
                    .collect::<Vec<_>>();
                for mut drop in promoted {
                    drops.remove(&drop);
                    drop.subtransaction_id = parent_id;
                    drops.insert(drop);
                }
            });
        }
        _ => {}
    }
}

fn mapped_drop_marker_bucket(marker: MappedIndexDropMarker) -> usize {
    (marker.index_oid as usize ^ marker.transaction_id.into_inner() as usize)
        % MAPPED_DROP_BUCKET_COUNT
}

fn mapped_drop_marker_path_in_bucket(
    marker: MappedIndexDropMarker,
    bucket: usize,
) -> Option<std::path::PathBuf> {
    mapped_drop_marker_path_in_directory(marker, bucket, MAPPED_DROP_MARKER_DIRECTORY)
}

fn mapped_drop_retry_path_in_bucket(
    marker: MappedIndexDropMarker,
    bucket: usize,
) -> Option<std::path::PathBuf> {
    mapped_drop_marker_path_in_directory(marker, bucket, MAPPED_DROP_RETRY_DIRECTORY)
}

fn mapped_drop_marker_path_in_directory(
    marker: MappedIndexDropMarker,
    bucket: usize,
    marker_directory: &str,
) -> Option<std::path::PathBuf> {
    hnsw_mapped_database_directory(marker.database_oid).map(|directory| {
        directory
            .join(marker_directory)
            .join(format!("{bucket:02x}"))
            .join(format!(
                "{MAPPED_DROP_MARKER_PREFIX}{}_{}{MAPPED_DROP_MARKER_SUFFIX}",
                marker.index_oid, marker.transaction_id
            ))
    })
}

fn mapped_drop_marker_path(marker: MappedIndexDropMarker) -> Option<std::path::PathBuf> {
    mapped_drop_marker_path_in_bucket(marker, mapped_drop_marker_bucket(marker))
}

fn existing_mapped_drop_marker_paths(
    marker: MappedIndexDropMarker,
) -> impl Iterator<Item = std::path::PathBuf> {
    (0..MAPPED_DROP_BUCKET_COUNT)
        .flat_map(move |bucket| {
            [
                mapped_drop_marker_path_in_bucket(marker, bucket),
                mapped_drop_retry_path_in_bucket(marker, bucket),
            ]
        })
        .flatten()
        .filter(|path| path.is_file())
}

fn persist_mapped_drop_marker(marker: MappedIndexDropMarker) -> std::io::Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write;

    let Some(path) = mapped_drop_marker_path(marker) else {
        return Ok(());
    };
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    ensure_durable_directory(parent)?;
    let index_directory = hnsw_mapped_index_directory(marker.database_oid, marker.index_oid)
        .ok_or_else(|| std::io::Error::other("mapped index directory is unavailable"))?;
    let mut generations = Vec::new();
    for entry in std::fs::read_dir(index_directory)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
        if parse_hnsw_mapped_generation_name(&name).is_some_and(|identity| {
            identity.database_oid == marker.database_oid && identity.index_oid == marker.index_oid
        }) {
            generations.push(name);
        }
    }
    generations.sort();
    let root = hnsw_mapped_database_directory(marker.database_oid)
        .ok_or_else(|| std::io::Error::other("mapped database directory is unavailable"))?;
    let temporary_directory = root.join(MAPPED_DROP_TEMP_DIRECTORY);
    ensure_durable_directory(&temporary_directory)?;
    let temporary = temporary_directory.join(format!(
        "{MAPPED_DROP_MARKER_PREFIX}{}_{}.tmp.{}",
        marker.index_oid,
        marker.transaction_id,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&temporary);
    let mut file = OpenOptions::new().write(true).create_new(true).open(&temporary)?;
    file.write_all(b"pgcontext-mapped-drop-v1\n")?;
    for generation in generations {
        writeln!(file, "{generation}")?;
    }
    file.sync_all()?;
    std::fs::rename(&temporary, &path)?;
    std::fs::File::open(parent)?.sync_all()?;
    std::fs::File::open(temporary_directory)?.sync_all()
}

fn ensure_durable_directory(path: &std::path::Path) -> std::io::Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("directory has no parent"))?;
    ensure_durable_directory(parent)?;
    match std::fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => {}
        Err(error) => return Err(error),
    }
    std::fs::File::open(path)?.sync_all()?;
    std::fs::File::open(parent)?.sync_all()
}

fn remove_mapped_drop_marker(marker: MappedIndexDropMarker) {
    for path in existing_mapped_drop_marker_paths(marker) {
        if let Err(error) = std::fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            pgrx::debug1!(
                "pgcontext mapped HNSW drop-marker cleanup failed for {}: {error}",
                path.display()
            );
        }
    }
}

fn parse_mapped_drop_marker_name(database_oid: u32, name: &str) -> Option<MappedIndexDropMarker> {
    let fields = name
        .strip_prefix(MAPPED_DROP_MARKER_PREFIX)?
        .strip_suffix(MAPPED_DROP_MARKER_SUFFIX)?;
    let (index_oid, transaction_id) = fields.split_once('_')?;
    Some(MappedIndexDropMarker {
        database_oid,
        index_oid: index_oid.parse().ok()?,
        transaction_id: pg_sys::TransactionId::from(transaction_id.parse::<u32>().ok()?),
    })
}

fn cleanup_committed_mapped_index_drop(marker: MappedIndexDropMarker) {
    if existing_mapped_drop_marker_paths(marker)
        .any(|path| cleanup_recorded_mapped_generations(marker, &path))
    {
        remove_mapped_drop_marker(marker);
    }
}

fn reconcile_pending_mapped_drops(database_oid: u32) {
    let Some(directory) = hnsw_mapped_database_directory(database_oid) else {
        return;
    };
    cleanup_stale_mapped_drop_temps(database_oid, &directory);
    let Some(bucket) = claim_mapped_drop_bucket(&directory) else { return };
    let marker_directory = directory
        .join(MAPPED_DROP_MARKER_DIRECTORY)
        .join(format!("{bucket:02x}"));
    let mut visited = 0_usize;
    if let Ok(entries) = std::fs::read_dir(&marker_directory) {
        for entry in entries.take(MAPPED_DROP_RECONCILE_BUDGET) {
            visited = visited.saturating_add(1);
            let Ok(entry) = entry else { continue };
            let Ok(file_type) = entry.file_type() else { continue };
            if !file_type.is_file() || file_type.is_symlink() { continue }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
            let path = entry.path();
            let Some(marker) = parse_mapped_drop_marker_name(database_oid, &name) else {
                if name.starts_with(MAPPED_DROP_MARKER_PREFIX) && name.contains(".tmp.") {
                    let _ = std::fs::remove_file(path);
                }
                continue;
            };
            // SAFETY: reconciliation runs inside a normal backend transaction
            // and passes only validated numeric marker fields.
            let completed = unsafe { reconcile_mapped_drop_marker(marker, &path) };
            if !completed {
                defer_mapped_drop_marker(marker, &path, bucket);
            }
        }
    }
    if visited < MAPPED_DROP_RECONCILE_BUDGET {
        requeue_deferred_mapped_drop_markers(database_oid, bucket, &marker_directory);
    }
}

fn claim_mapped_drop_bucket(directory: &std::path::Path) -> Option<usize> {
    use std::io::Write;
    let result = (|| -> std::io::Result<usize> {
        let cursor_path = directory.join(MAPPED_DROP_CURSOR_NAME);
        let lock_path = directory.join(MAPPED_DROP_CURSOR_LOCK_NAME);
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        lock.lock()?;
        let bucket = std::fs::read_to_string(&cursor_path)
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|bucket| *bucket < MAPPED_DROP_BUCKET_COUNT)
            .unwrap_or_default();
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&cursor_path)?;
        write!(file, "{}", (bucket + 1) % MAPPED_DROP_BUCKET_COUNT)?;
        file.sync_all()?;
        std::fs::File::open(directory)?.sync_all()?;
        lock.unlock()?;
        Ok(bucket)
    })();
    match result {
        Ok(bucket) => Some(bucket),
        Err(error) => {
            pgrx::debug1!("pgcontext mapped HNSW reconciliation cursor claim failed: {error}");
            None
        }
    }
}

fn cleanup_stale_mapped_drop_temps(database_oid: u32, directory: &std::path::Path) {
    let temporary_directory = directory.join(MAPPED_DROP_TEMP_DIRECTORY);
    let Ok(entries) = std::fs::read_dir(&temporary_directory) else { return };
    let mut removed = false;
    for entry in entries.take(MAPPED_DROP_RECONCILE_BUDGET).flatten() {
        let Ok(file_type) = entry.file_type() else { continue };
        if !file_type.is_file() || file_type.is_symlink() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
        let Some(marker) = parse_mapped_drop_temp_name(database_oid, &name) else { continue };
        // SAFETY: the filename parser validates the numeric transaction ID;
        // this runs in a normal backend transaction.
        if unsafe { pg_sys::TransactionIdIsInProgress(marker.transaction_id) } {
            continue;
        }
        if std::fs::remove_file(entry.path()).is_ok() {
            removed = true;
        }
    }
    if removed
        && let Err(error) = std::fs::File::open(&temporary_directory)
            .and_then(|file| file.sync_all())
    {
        pgrx::debug1!("pgcontext mapped HNSW temp cleanup sync failed: {error}");
    }
}

fn parse_mapped_drop_temp_name(database_oid: u32, name: &str) -> Option<MappedIndexDropMarker> {
    let fields = name.strip_prefix(MAPPED_DROP_MARKER_PREFIX)?;
    let (marker_fields, process_id) = fields.split_once(".tmp.")?;
    process_id.parse::<u32>().ok()?;
    let (index_oid, transaction_id) = marker_fields.split_once('_')?;
    Some(MappedIndexDropMarker {
        database_oid,
        index_oid: index_oid.parse().ok()?,
        transaction_id: pg_sys::TransactionId::from(transaction_id.parse::<u32>().ok()?),
    })
}

fn defer_mapped_drop_marker(
    marker: MappedIndexDropMarker,
    path: &std::path::Path,
    bucket: usize,
) {
    let Some(destination) = mapped_drop_retry_path_in_bucket(marker, bucket) else { return };
    let Some(parent) = destination.parent() else { return };
    let source_parent = path.parent();
    let result = (|| -> std::io::Result<()> {
        ensure_durable_directory(parent)?;
        std::fs::rename(path, &destination)?;
        std::fs::File::open(parent)?.sync_all()?;
        if let Some(source_parent) = source_parent {
            std::fs::File::open(source_parent)?.sync_all()?;
        }
        Ok(())
    })();
    if let Err(error) = result
        && error.kind() != std::io::ErrorKind::NotFound
    {
        pgrx::debug1!("pgcontext mapped HNSW drop-marker deferral failed: {error}");
    }
}

fn requeue_deferred_mapped_drop_markers(
    database_oid: u32,
    bucket: usize,
    marker_directory: &std::path::Path,
) {
    let Some(root) = hnsw_mapped_database_directory(database_oid) else { return };
    let retry_directory = root
        .join(MAPPED_DROP_RETRY_DIRECTORY)
        .join(format!("{bucket:02x}"));
    let Ok(entries) = std::fs::read_dir(&retry_directory) else { return };
    if ensure_durable_directory(marker_directory).is_err() {
        return;
    }
    let mut moved = false;
    for entry in entries.take(MAPPED_DROP_RECONCILE_BUDGET).flatten() {
        let Ok(file_type) = entry.file_type() else { continue };
        if !file_type.is_file() || file_type.is_symlink() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
        if parse_mapped_drop_marker_name(database_oid, &name).is_none() {
            continue;
        }
        if std::fs::rename(entry.path(), marker_directory.join(name)).is_ok() {
            moved = true;
        }
    }
    if moved {
        if let Err(error) = std::fs::File::open(marker_directory).and_then(|file| file.sync_all()) {
            pgrx::debug1!("pgcontext mapped HNSW pending-lane sync failed: {error}");
        }
        if let Err(error) =
            std::fs::File::open(retry_directory).and_then(|file| file.sync_all())
        {
            pgrx::debug1!("pgcontext mapped HNSW retry-lane sync failed: {error}");
        }
    }
}

unsafe fn reconcile_mapped_drop_marker(
    marker: MappedIndexDropMarker,
    path: &std::path::Path,
) -> bool {
    // SAFETY: these transaction-status and syscache lookups run in a normal
    // backend transaction. The syscache tuple is released before returning.
    let (in_progress, did_abort) = unsafe {
        let in_progress = pg_sys::TransactionIdIsInProgress(marker.transaction_id);
        let did_abort = !in_progress && pg_sys::TransactionIdDidAbort(marker.transaction_id);
        (in_progress, did_abort)
    };
    pgrx::debug1!(
        "pgcontext mapped HNSW drop-marker reconcile index={} xid={} in_progress={} did_abort={}",
        marker.index_oid,
        marker.transaction_id,
        in_progress,
        did_abort
    );
    if in_progress {
        return false;
    }
    if did_abort {
        remove_mapped_drop_marker(marker);
        true
    } else {
        // Exact generation names in the marker make this safe even after OID
        // reuse. If very old commit-status detail was truncated, deleting an
        // aborted index's derived mapped cache only forces a page-backed rebuild.
        let cleaned = cleanup_recorded_mapped_generations(marker, path);
        if cleaned {
            remove_mapped_drop_marker(marker);
        }
        cleaned
    }
}

fn cleanup_recorded_mapped_generations(
    marker: MappedIndexDropMarker,
    marker_path: &std::path::Path,
) -> bool {
    let Ok(payload) = std::fs::read_to_string(marker_path) else { return false };
    let mut lines = payload.lines();
    if lines.next() != Some("pgcontext-mapped-drop-v1") {
        return false;
    }
    let Some(index_directory) = hnsw_mapped_index_directory(marker.database_oid, marker.index_oid)
    else { return false };
    for generation in lines {
        let Some(identity) = parse_hnsw_mapped_generation_name(generation) else { return false };
        if identity.database_oid != marker.database_oid || identity.index_oid != marker.index_oid {
            return false;
        }
        let path = index_directory.join(generation);
        if let Err(error) = std::fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return false;
        }
    }
    if index_directory.is_dir() {
        let _ = std::fs::remove_dir(&index_directory);
    }
    let Some(parent) = index_directory.parent() else { return false };
    std::fs::File::open(parent).and_then(|file| file.sync_all()).is_ok()
}
