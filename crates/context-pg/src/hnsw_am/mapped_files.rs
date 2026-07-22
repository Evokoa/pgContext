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
    // SAFETY: PostgreSQL initializes `DataDir` before loading extensions and
    // retains the NUL-terminated allocation for the backend lifetime.
    let data_dir = unsafe {
        if pg_sys::DataDir.is_null() {
            return None;
        }
        CStr::from_ptr(pg_sys::DataDir).to_bytes()
    };
    let data_dir = std::str::from_utf8(data_dir).ok()?;
    Some(std::path::PathBuf::from(data_dir).join("pgcontext_hnsw_mapped").join(format!(
        "{}_{}_{}_{}_{}.pgctxseg",
        identity.database_oid,
        identity.index_oid,
        identity.rel_file_number,
        identity.directory_epoch,
        identity.meta_lsn
    )))
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
