//! Pure lifecycle tests for automatic document chunking.

use context_build::{
    DocumentChunkJob, DocumentChunkJobError, DocumentChunkJobStatus, DocumentChunkLease,
};

fn job() -> Result<DocumentChunkJob, DocumentChunkJobError> {
    DocumentChunkJob::new(3, 7, 9, 1_000)
}

#[test]
fn lease_takeover_fences_stale_workers_and_restarts_from_leased()
-> Result<(), DocumentChunkJobError> {
    let mut job = job()?;
    let first = job.claim(100, 20, 11)?;
    job.advance(first, 100, DocumentChunkJobStatus::Parsing, 4)?;
    assert_eq!(
        job.claim(119, 20, 12),
        Err(DocumentChunkJobError::LeaseHeld)
    );
    let second = job.claim(120, 20, 12)?;
    assert_eq!(job.status(), DocumentChunkJobStatus::Leased);
    assert_eq!(job.processed_units(), 0);
    assert_eq!(job.attempt(), 2);
    assert_eq!(
        job.advance(first, 120, DocumentChunkJobStatus::Chunking, 5),
        Err(DocumentChunkJobError::StaleLease)
    );
    job.advance(second, 120, DocumentChunkJobStatus::Parsing, 1)?;
    Ok(())
}

#[test]
fn expired_cancel_request_terminalizes_without_takeover() -> Result<(), DocumentChunkJobError> {
    let mut job = job()?;
    let first = job.claim(100, 20, 11)?;
    job.cancel()?;
    assert_eq!(
        job.claim(119, 20, 12),
        Err(DocumentChunkJobError::LeaseHeld)
    );
    assert_eq!(
        job.claim(120, 20, 12),
        Err(DocumentChunkJobError::InvalidTransition)
    );
    assert_eq!(job.status(), DocumentChunkJobStatus::Cancelled);
    assert_eq!(
        job.finish_cancellation(first, 120),
        Err(DocumentChunkJobError::StaleLease)
    );
    Ok(())
}

#[test]
fn heartbeat_acknowledges_cancel_request_as_terminal() -> Result<(), DocumentChunkJobError> {
    let mut job = job()?;
    let lease = job.claim(100, 20, 11)?;
    job.cancel()?;
    assert_eq!(
        job.heartbeat(lease, 20, 110),
        Err(DocumentChunkJobError::InvalidTransition)
    );
    assert_eq!(job.status(), DocumentChunkJobStatus::Cancelled);
    assert_eq!(
        job.finish_cancellation(lease, 110),
        Err(DocumentChunkJobError::StaleLease)
    );
    Ok(())
}

#[test]
fn legal_pipeline_publishes_once_and_identical_replay_converges()
-> Result<(), DocumentChunkJobError> {
    let mut job = job()?;
    let lease = job.claim(0, 60, 1)?;
    for (status, progress) in [
        (DocumentChunkJobStatus::Parsing, 1),
        (DocumentChunkJobStatus::Chunking, 3),
        (DocumentChunkJobStatus::Embedding, 5),
        (DocumentChunkJobStatus::Validating, 7),
        (DocumentChunkJobStatus::Publishing, 9),
    ] {
        job.advance(lease, 1, status, progress)?;
    }
    job.publish(lease, 1, 44)?;
    job.publish(lease, 1, 44)?;
    assert_eq!(job.status(), DocumentChunkJobStatus::Ready);
    assert_eq!(
        job.publish(lease, 1, 45),
        Err(DocumentChunkJobError::ConflictingPublication)
    );
    Ok(())
}

#[test]
fn cancellation_failure_retry_and_supersession_preserve_prior_ready()
-> Result<(), DocumentChunkJobError> {
    let mut job = job()?;
    let lease = job.claim(0, 60, 1)?;
    job.cancel()?;
    job.finish_cancellation(lease, 1)?;
    job.retry()?;
    assert_eq!(job.status(), DocumentChunkJobStatus::Queued);

    let lease = job.claim(100, 60, 2)?;
    job.fail(lease, 101)?;
    job.retry()?;
    job.supersede(8)?;
    assert_eq!(job.status(), DocumentChunkJobStatus::Superseded);
    assert_eq!(job.prior_ready_generation(), Some(1_000));
    Ok(())
}

#[test]
fn progress_retry_lease_and_source_bounds_fail_closed() -> Result<(), DocumentChunkJobError> {
    assert!(DocumentChunkJob::new(0, 7, 9, 1_000).is_err());
    let mut job = job()?;
    assert_eq!(job.claim(0, 0, 1), Err(DocumentChunkJobError::InvalidLease));
    let lease = job.claim(0, 60, 1)?;
    assert_eq!(
        job.advance(lease, 1, DocumentChunkJobStatus::Parsing, 10),
        Err(DocumentChunkJobError::ProgressExceedsTotal)
    );
    assert_eq!(
        job.heartbeat(lease, 60, 61),
        Err(DocumentChunkJobError::LeaseExpired)
    );
    assert_eq!(
        job.supersede(3),
        Err(DocumentChunkJobError::SourceVersionNotNewer)
    );
    Ok(())
}

#[test]
fn every_worker_boundary_rejects_an_expired_lease() -> Result<(), DocumentChunkJobError> {
    let mut advancing = job()?;
    let lease = advancing.claim(0, 10, 1)?;
    assert_eq!(
        advancing.advance(lease, 10, DocumentChunkJobStatus::Parsing, 1),
        Err(DocumentChunkJobError::LeaseExpired)
    );

    let mut publishing = job()?;
    let lease = publishing.claim(0, 10, 1)?;
    for (status, progress) in [
        (DocumentChunkJobStatus::Parsing, 1),
        (DocumentChunkJobStatus::Chunking, 3),
        (DocumentChunkJobStatus::Embedding, 5),
        (DocumentChunkJobStatus::Validating, 7),
        (DocumentChunkJobStatus::Publishing, 9),
    ] {
        publishing.advance(lease, 1, status, progress)?;
    }
    assert_eq!(
        publishing.publish(lease, 10, 44),
        Err(DocumentChunkJobError::LeaseExpired)
    );

    let mut cancelling = job()?;
    let lease = cancelling.claim(0, 10, 1)?;
    cancelling.cancel()?;
    assert_eq!(
        cancelling.finish_cancellation(lease, 10),
        Err(DocumentChunkJobError::LeaseExpired)
    );

    let mut failing = job()?;
    let lease = failing.claim(0, 10, 1)?;
    assert_eq!(
        failing.fail(lease, 10),
        Err(DocumentChunkJobError::LeaseExpired)
    );
    Ok(())
}

#[test]
fn lease_value_is_opaque_and_nonzero() {
    assert!(DocumentChunkLease::new(0, 1).is_none());
    assert_eq!(
        DocumentChunkLease::new(2, 3).map(DocumentChunkLease::token),
        Some(2)
    );
}
