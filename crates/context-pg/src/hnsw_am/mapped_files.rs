// Immutable full-layer HNSW graph generation files used by the AM.

fn hnsw_mapped_identity(
    database_oid: u32,
    index_oid: u32,
    rel_file_number: u32,
    directory_epoch: u64,
    meta_lsn: u64,
) -> MappedGraphIdentity {
    MappedGraphIdentity {
        database_oid,
        index_oid,
        rel_file_number,
        directory_epoch,
        meta_lsn,
    }
}

fn hnsw_mapped_generation_path(identity: MappedGraphIdentity) -> Option<std::path::PathBuf> {
    let directory = hnsw_mapped_index_directory(identity.database_oid, identity.index_oid)?;
    Some(directory.join(format!(
        "{}_{}_{}_{}_{}.pgctxseg",
        identity.database_oid,
        identity.index_oid,
        identity.rel_file_number,
        identity.directory_epoch,
        identity.meta_lsn
    )))
}

fn hnsw_mapped_index_directory(database_oid: u32, index_oid: u32) -> Option<std::path::PathBuf> {
    hnsw_mapped_database_directory(database_oid).map(|directory| directory.join(index_oid.to_string()))
}

fn hnsw_mapped_database_directory(database_oid: u32) -> Option<std::path::PathBuf> {
    // A generation is meaningful only in the connected database. Keeping the
    // files below PostgreSQL's physical database directory makes DROP DATABASE
    // reclaim them with the database itself, including non-default tablespaces.
    // SAFETY: PostgreSQL initializes these globals before loading extensions.
    // `GetDatabasePath` returns a palloc-owned NUL-terminated path for the
    // current database/tablespace pair; the bytes are copied before `pfree`.
    let database_path = unsafe {
        if pg_sys::MyDatabaseId.to_u32() != database_oid {
            return None;
        }
        let path = pg_sys::GetDatabasePath(pg_sys::MyDatabaseId, pg_sys::MyDatabaseTableSpace);
        if path.is_null() {
            return None;
        }
        let bytes = CStr::from_ptr(path).to_bytes().to_vec();
        pg_sys::pfree(path.cast());
        std::str::from_utf8(&bytes).ok().map(std::path::PathBuf::from)?
    };
    let database_path = if database_path.is_absolute() {
        database_path
    } else {
        postgres_data_directory()?.join(database_path)
    };
    Some(database_path.join("pgcontext_hnsw_mapped"))
}

fn postgres_data_directory() -> Option<std::path::PathBuf> {
    // SAFETY: PostgreSQL retains the initialized DataDir allocation for the
    // backend lifetime; the path bytes are copied into an owned PathBuf.
    unsafe {
        if pg_sys::DataDir.is_null() {
            return None;
        }
        let bytes = CStr::from_ptr(pg_sys::DataDir).to_bytes();
        std::str::from_utf8(bytes)
            .ok()
            .map(std::path::PathBuf::from)
    }
}

fn parse_hnsw_mapped_generation_name(name: &str) -> Option<MappedGraphIdentity> {
    let stem = name.strip_suffix(".pgctxseg")?;
    let mut fields = stem.split('_');
    let identity = MappedGraphIdentity {
        database_oid: fields.next()?.parse().ok()?,
        index_oid: fields.next()?.parse().ok()?,
        rel_file_number: fields.next()?.parse().ok()?,
        directory_epoch: fields.next()?.parse().ok()?,
        meta_lsn: fields.next()?.parse().ok()?,
    };
    fields.next().is_none().then_some(identity)
}

/// Retires older generations for this logical index after a current generation
/// has been durably published. A stale backend that loses a publication race
/// to a higher metapage LSN removes its own file instead of retiring the newer
/// one. Unlink is mapping-safe: existing readers retain the open inode until
/// their `MappedPackedGraphImage` owner drops.
fn retire_stale_mapped_generations(
    current_path: &std::path::Path,
    current: MappedGraphIdentity,
) -> bool {
    let Some(parent) = current_path.parent() else {
        return true;
    };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return true;
    };
    let mut older_paths = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path == current_path {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(candidate) = parse_hnsw_mapped_generation_name(name) else {
            continue;
        };
        if candidate.database_oid != current.database_oid || candidate.index_oid != current.index_oid
        {
            continue;
        }
        if candidate.meta_lsn > current.meta_lsn {
            let _ = std::fs::remove_file(current_path);
            return false;
        }
        if candidate.meta_lsn < current.meta_lsn {
            older_paths.push(path);
        }
    }
    for path in older_paths {
        if let Err(error) = std::fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            pgrx::debug1!(
                "pgcontext mapped HNSW stale-generation cleanup failed for {}: {error}",
                path.display()
            );
        }
    }
    true
}

fn attach_mapped_packed_image(
    identity: MappedGraphIdentity,
    budget_bytes: u64,
) -> Option<MappedPackedGraphImage> {
    let path = hnsw_mapped_generation_path(identity)?;
    let encoded_bytes = std::fs::metadata(&path).ok()?.len();
    if encoded_bytes > budget_bytes {
        return None;
    }
    // SAFETY: AM generations are written once under an identity-derived name.
    // Publication atomically installs a new inode and cleanup only unlinks
    // stale names; no code mutates or truncates an installed generation.
    match unsafe { MappedPackedGraphImage::open(&path, identity) } {
        Ok(image) => Some(image),
        Err(error) => {
            pgrx::debug1!("pgcontext mapped HNSW attach failed: {error}");
            None
        }
    }
}

fn publish_mapped_packed_image(
    identity: MappedGraphIdentity,
    image: &[u8],
    budget_bytes: u64,
) -> bool {
    let payload = encode_mapped_packed_graph(identity, image);
    let encoded_len = SegmentHeader::ENCODED_LEN.saturating_add(payload.len());
    if u64::try_from(encoded_len).unwrap_or(u64::MAX) > budget_bytes {
        return false;
    }
    let Some(path) = hnsw_mapped_generation_path(identity) else {
        return false;
    };
    let Some(parent) = path.parent() else {
        return false;
    };
    if let Err(error) = std::fs::create_dir_all(parent) {
        pgrx::debug1!("pgcontext mapped HNSW directory creation failed: {error}");
        return false;
    }
    if let Err(error) = write_segment_atomic(&path, SegmentKind::HnswGraph, &payload) {
        pgrx::debug1!("pgcontext mapped HNSW publish failed: {error}");
        return false;
    }
    // SAFETY: the just-published generation is immutable after its atomic
    // rename; later generation replacement uses a different identity/name.
    match unsafe { MappedPackedGraphImage::open(&path, identity) } {
        Ok(_) => retire_stale_mapped_generations(&path, identity),
        Err(error) => {
            pgrx::debug1!("pgcontext mapped HNSW publish validation failed: {error}");
            false
        }
    }
}

#[cfg(feature = "pg_test")]
pub(crate) fn mapped_generation_paths_for_test(index_oid: u32) -> Vec<std::path::PathBuf> {
    // SAFETY: PostgreSQL initializes MyDatabaseId before any pg_test executes.
    let database_oid = unsafe { pg_sys::MyDatabaseId.to_u32() };
    let Some(directory) = hnsw_mapped_index_directory(database_oid, index_oid) else {
        return Vec::new();
    };
    let mut paths = std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .and_then(parse_hnsw_mapped_generation_name)
                .is_some_and(|identity| {
                    identity.database_oid == database_oid && identity.index_oid == index_oid
                })
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}
