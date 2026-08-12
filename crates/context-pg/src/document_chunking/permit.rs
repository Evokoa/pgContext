//! Backend-local one-shot authority for SECURITY DEFINER helpers.

use std::cell::RefCell;

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DocumentChunkPermitKind {
    Stage,
    Complete,
    RegisterProfile,
    RegisterSource,
    LoadStaging,
    Rollback,
    Fail,
    ReleaseClaim,
    Checkpoint,
    LoadJob,
    Enqueue,
    Claim,
    InstallTrigger,
    Heartbeat,
    Cancel,
    Retry,
    Invalidate,
    Rebuild,
    SupersedeClaim,
    PromoteProfileAlias,
    RollbackProfileAlias,
    PrepareProfileAlias,
    DrainProfileAlias,
    LoadClaimSource,
    LockSource,
    LockJobAlias,
    LockReadAlias,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DocumentChunkPermit {
    kind: DocumentChunkPermitKind,
    identity_a: i64,
    identity_b: i64,
}

thread_local! {
    static DOCUMENT_CHUNK_PERMIT: RefCell<Option<DocumentChunkPermit>> =
        const { RefCell::new(None) };
}

pub(super) fn arm_document_chunk_permit(
    kind: DocumentChunkPermitKind,
    identity_a: i64,
    identity_b: i64,
) {
    DOCUMENT_CHUNK_PERMIT.with(|permit| {
        *permit.borrow_mut() = Some(DocumentChunkPermit {
            kind,
            identity_a,
            identity_b,
        });
    });
}

/// Consumes the exact one-shot authority armed by the validated Rust entrypoint.
#[pg_extern(name = "_consume_document_chunk_permit")]
#[search_path(pg_catalog, pgcontext)]
fn consume_document_chunk_permit(operation: i32, identity_a: i64, identity_b: i64) -> bool {
    let kind = match operation {
        1 => DocumentChunkPermitKind::Stage,
        2 => DocumentChunkPermitKind::Complete,
        3 => DocumentChunkPermitKind::RegisterProfile,
        4 => DocumentChunkPermitKind::RegisterSource,
        5 => DocumentChunkPermitKind::LoadStaging,
        6 => DocumentChunkPermitKind::Rollback,
        7 => DocumentChunkPermitKind::Fail,
        8 => DocumentChunkPermitKind::ReleaseClaim,
        9 => DocumentChunkPermitKind::Checkpoint,
        10 => DocumentChunkPermitKind::LoadJob,
        11 => DocumentChunkPermitKind::Enqueue,
        12 => DocumentChunkPermitKind::Claim,
        13 => DocumentChunkPermitKind::InstallTrigger,
        14 => DocumentChunkPermitKind::Heartbeat,
        15 => DocumentChunkPermitKind::Cancel,
        16 => DocumentChunkPermitKind::Retry,
        17 => DocumentChunkPermitKind::Invalidate,
        18 => DocumentChunkPermitKind::Rebuild,
        19 => DocumentChunkPermitKind::SupersedeClaim,
        20 => DocumentChunkPermitKind::PromoteProfileAlias,
        21 => DocumentChunkPermitKind::RollbackProfileAlias,
        22 => DocumentChunkPermitKind::PrepareProfileAlias,
        23 => DocumentChunkPermitKind::DrainProfileAlias,
        24 => DocumentChunkPermitKind::LoadClaimSource,
        25 => DocumentChunkPermitKind::LockSource,
        26 => DocumentChunkPermitKind::LockJobAlias,
        27 => DocumentChunkPermitKind::LockReadAlias,
        _ => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "document chunk publication operation is invalid",
        ),
    };
    let authorized = DOCUMENT_CHUNK_PERMIT.with(|permit| {
        permit.borrow_mut().take()
            == Some(DocumentChunkPermit {
                kind,
                identity_a,
                identity_b,
            })
    });
    if !authorized {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            "document chunk helper requires validated entrypoint authority",
        );
    }
    true
}
