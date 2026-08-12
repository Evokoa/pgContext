//! Pure fenced lifecycle for one source-version chunk generation.

use std::fmt;

/// Maximum attempts for one idempotent source-version/profile job.
pub const MAX_DOCUMENT_CHUNK_JOB_ATTEMPTS: u32 = 3;
/// Maximum logical lease duration accepted by the pure contract.
pub const MAX_DOCUMENT_CHUNK_LEASE_TICKS: u64 = 60_000;

/// Durable automatic-chunking lifecycle state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentChunkJobStatus {
    /// Ready to be claimed.
    Queued,
    /// A worker owns the lease but has not recorded a stage yet.
    Leased,
    /// Source parsing is in progress.
    Parsing,
    /// Deterministic chunk construction is in progress.
    Chunking,
    /// Derived embedding work is in progress.
    Embedding,
    /// Staged output is being validated.
    Validating,
    /// One short atomic publication is in progress.
    Publishing,
    /// Complete generation is current and query-visible.
    Ready,
    /// Current worker must stop at its next checkpoint.
    CancelRequested,
    /// Work stopped without replacing the prior ready generation.
    Cancelled,
    /// Current attempt failed without replacing prior ready data.
    Failed,
    /// A newer source version invalidated this job.
    Superseded,
    /// Published generation was explicitly retired.
    Retired,
}

impl DocumentChunkJobStatus {
    /// Returns the stable catalog representation.
    #[must_use]
    pub const fn as_catalog(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Leased => "leased",
            Self::Parsing => "parsing",
            Self::Chunking => "chunking",
            Self::Embedding => "embedding",
            Self::Validating => "validating",
            Self::Publishing => "publishing",
            Self::Ready => "ready",
            Self::CancelRequested => "cancel_requested",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
            Self::Superseded => "superseded",
            Self::Retired => "retired",
        }
    }

    /// Parses a stable catalog value.
    #[must_use]
    pub const fn from_catalog(value: &str) -> Option<Self> {
        match value.as_bytes() {
            b"queued" => Some(Self::Queued),
            b"leased" => Some(Self::Leased),
            b"parsing" => Some(Self::Parsing),
            b"chunking" => Some(Self::Chunking),
            b"embedding" => Some(Self::Embedding),
            b"validating" => Some(Self::Validating),
            b"publishing" => Some(Self::Publishing),
            b"ready" => Some(Self::Ready),
            b"cancel_requested" => Some(Self::CancelRequested),
            b"cancelled" => Some(Self::Cancelled),
            b"failed" => Some(Self::Failed),
            b"superseded" => Some(Self::Superseded),
            b"retired" => Some(Self::Retired),
            _ => None,
        }
    }

    const fn allows_advance(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Leased, Self::Parsing)
                | (Self::Parsing, Self::Chunking)
                | (Self::Chunking, Self::Embedding)
                | (Self::Embedding, Self::Validating)
                | (Self::Validating, Self::Publishing)
        )
    }

    const fn is_active(self) -> bool {
        matches!(
            self,
            Self::Leased
                | Self::Parsing
                | Self::Chunking
                | Self::Embedding
                | Self::Validating
                | Self::Publishing
                | Self::CancelRequested
        )
    }
}

/// Opaque worker lease token and exclusive expiry instant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentChunkLease {
    token: u64,
    expires_at: u64,
}

impl DocumentChunkLease {
    /// Creates a non-zero lease identity with a positive expiry instant.
    #[must_use]
    pub const fn new(token: u64, expires_at: u64) -> Option<Self> {
        if token == 0 || expires_at == 0 {
            None
        } else {
            Some(Self { token, expires_at })
        }
    }

    /// Returns the opaque fencing token.
    #[must_use]
    pub const fn token(self) -> u64 {
        self.token
    }

    /// Returns the exclusive logical expiry instant.
    #[must_use]
    pub const fn expires_at(self) -> u64 {
        self.expires_at
    }
}

/// Pure durable state for one document/profile/source-version job.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentChunkJob {
    source_version: u64,
    profile_revision: u64,
    status: DocumentChunkJobStatus,
    attempt: u32,
    processed_units: u64,
    total_units: u64,
    lease: Option<DocumentChunkLease>,
    prior_ready_generation: Option<u64>,
    publication_digest: Option<u64>,
}

impl DocumentChunkJob {
    /// Creates one queued job over a non-zero immutable source/profile pair.
    ///
    /// # Errors
    ///
    /// Returns [`DocumentChunkJobError::InvalidIdentity`] for zero identities
    /// or [`DocumentChunkJobError::ProgressExceedsTotal`] for zero work.
    pub const fn new(
        source_version: u64,
        profile_revision: u64,
        total_units: u64,
        prior_ready_generation: u64,
    ) -> Result<Self, DocumentChunkJobError> {
        if source_version == 0 || profile_revision == 0 {
            return Err(DocumentChunkJobError::InvalidIdentity);
        }
        if total_units == 0 {
            return Err(DocumentChunkJobError::ProgressExceedsTotal);
        }
        Ok(Self {
            source_version,
            profile_revision,
            status: DocumentChunkJobStatus::Queued,
            attempt: 0,
            processed_units: 0,
            total_units,
            lease: None,
            prior_ready_generation: if prior_ready_generation == 0 {
                None
            } else {
                Some(prior_ready_generation)
            },
            publication_digest: None,
        })
    }

    /// Claims queued work or takes over an expired active lease.
    ///
    /// # Errors
    ///
    /// Fails when another lease is current, the duration/token is invalid, the
    /// job is terminal, or its retry ceiling has been reached.
    pub fn claim(
        &mut self,
        now: u64,
        lease_ticks: u64,
        token: u64,
    ) -> Result<DocumentChunkLease, DocumentChunkJobError> {
        if lease_ticks == 0 || lease_ticks > MAX_DOCUMENT_CHUNK_LEASE_TICKS || token == 0 {
            return Err(DocumentChunkJobError::InvalidLease);
        }
        if self.status == DocumentChunkJobStatus::CancelRequested {
            match self.lease {
                Some(lease) if now < lease.expires_at => {
                    return Err(DocumentChunkJobError::LeaseHeld);
                }
                _ => {
                    self.status = DocumentChunkJobStatus::Cancelled;
                    self.lease = None;
                    return Err(DocumentChunkJobError::InvalidTransition);
                }
            }
        }
        if self.attempt >= MAX_DOCUMENT_CHUNK_JOB_ATTEMPTS {
            return Err(DocumentChunkJobError::RetryLimit);
        }
        match self.lease {
            Some(lease) if now < lease.expires_at => return Err(DocumentChunkJobError::LeaseHeld),
            _ if self.status == DocumentChunkJobStatus::Queued || self.status.is_active() => {}
            _ => return Err(DocumentChunkJobError::InvalidTransition),
        }
        let expires_at = now
            .checked_add(lease_ticks)
            .ok_or(DocumentChunkJobError::ArithmeticOverflow)?;
        let lease = DocumentChunkLease { token, expires_at };
        self.lease = Some(lease);
        self.attempt = self
            .attempt
            .checked_add(1)
            .ok_or(DocumentChunkJobError::ArithmeticOverflow)?;
        self.status = DocumentChunkJobStatus::Leased;
        self.processed_units = 0;
        Ok(lease)
    }

    /// Renews a current unexpired lease.
    ///
    /// # Errors
    ///
    /// Fails for a stale/expired lease or invalid duration.
    pub fn heartbeat(
        &mut self,
        lease: DocumentChunkLease,
        lease_ticks: u64,
        now: u64,
    ) -> Result<DocumentChunkLease, DocumentChunkJobError> {
        self.require_lease(lease, now)?;
        if self.status == DocumentChunkJobStatus::CancelRequested {
            self.status = DocumentChunkJobStatus::Cancelled;
            self.lease = None;
            return Err(DocumentChunkJobError::InvalidTransition);
        }
        if lease_ticks == 0 || lease_ticks > MAX_DOCUMENT_CHUNK_LEASE_TICKS {
            return Err(DocumentChunkJobError::InvalidLease);
        }
        let renewed = DocumentChunkLease {
            token: lease.token,
            expires_at: now
                .checked_add(lease_ticks)
                .ok_or(DocumentChunkJobError::ArithmeticOverflow)?,
        };
        self.lease = Some(renewed);
        Ok(renewed)
    }

    /// Advances one legal stage and monotonic progress checkpoint.
    ///
    /// # Errors
    ///
    /// Fails for stale leases, illegal stages, or progress outside the total.
    pub fn advance(
        &mut self,
        lease: DocumentChunkLease,
        now: u64,
        next: DocumentChunkJobStatus,
        processed_units: u64,
    ) -> Result<(), DocumentChunkJobError> {
        self.require_lease(lease, now)?;
        if processed_units > self.total_units || processed_units < self.processed_units {
            return Err(DocumentChunkJobError::ProgressExceedsTotal);
        }
        if self.status != next && !self.status.allows_advance(next) {
            return Err(DocumentChunkJobError::InvalidTransition);
        }
        self.status = next;
        self.processed_units = processed_units;
        Ok(())
    }

    /// Publishes one digest or accepts an identical replay.
    ///
    /// # Errors
    ///
    /// Fails for stale leases, an illegal stage, or a different replay digest.
    pub fn publish(
        &mut self,
        lease: DocumentChunkLease,
        now: u64,
        digest: u64,
    ) -> Result<(), DocumentChunkJobError> {
        self.require_lease(lease, now)?;
        if digest == 0 {
            return Err(DocumentChunkJobError::InvalidIdentity);
        }
        if self.status == DocumentChunkJobStatus::Ready {
            return if self.publication_digest == Some(digest) {
                Ok(())
            } else {
                Err(DocumentChunkJobError::ConflictingPublication)
            };
        }
        if self.status != DocumentChunkJobStatus::Publishing {
            return Err(DocumentChunkJobError::InvalidTransition);
        }
        self.publication_digest = Some(digest);
        self.status = DocumentChunkJobStatus::Ready;
        self.processed_units = self.total_units;
        Ok(())
    }

    /// Requests cooperative cancellation without discarding prior-ready data.
    ///
    /// # Errors
    ///
    /// Fails unless the job is active.
    pub fn cancel(&mut self) -> Result<(), DocumentChunkJobError> {
        if !self.status.is_active() {
            return Err(DocumentChunkJobError::InvalidTransition);
        }
        self.status = DocumentChunkJobStatus::CancelRequested;
        Ok(())
    }

    /// Records that the fenced worker honored cancellation.
    ///
    /// # Errors
    ///
    /// Fails for a stale lease or non-cancellation state.
    pub fn finish_cancellation(
        &mut self,
        lease: DocumentChunkLease,
        now: u64,
    ) -> Result<(), DocumentChunkJobError> {
        self.require_lease(lease, now)?;
        if self.status != DocumentChunkJobStatus::CancelRequested {
            return Err(DocumentChunkJobError::InvalidTransition);
        }
        self.status = DocumentChunkJobStatus::Cancelled;
        self.lease = None;
        Ok(())
    }

    /// Records one operational failure.
    ///
    /// # Errors
    ///
    /// Fails for a stale lease or terminal state.
    pub fn fail(
        &mut self,
        lease: DocumentChunkLease,
        now: u64,
    ) -> Result<(), DocumentChunkJobError> {
        self.require_lease(lease, now)?;
        if !self.status.is_active() {
            return Err(DocumentChunkJobError::InvalidTransition);
        }
        self.status = DocumentChunkJobStatus::Failed;
        self.lease = None;
        Ok(())
    }

    /// Requeues cancelled or failed work within the attempt ceiling.
    ///
    /// # Errors
    ///
    /// Fails for another state or after the retry ceiling.
    pub fn retry(&mut self) -> Result<(), DocumentChunkJobError> {
        if !matches!(
            self.status,
            DocumentChunkJobStatus::Cancelled | DocumentChunkJobStatus::Failed
        ) {
            return Err(DocumentChunkJobError::InvalidTransition);
        }
        if self.attempt >= MAX_DOCUMENT_CHUNK_JOB_ATTEMPTS {
            return Err(DocumentChunkJobError::RetryLimit);
        }
        self.status = DocumentChunkJobStatus::Queued;
        Ok(())
    }

    /// Marks work superseded by a strictly newer source version.
    ///
    /// # Errors
    ///
    /// Fails unless the supplied version is strictly newer or the job is
    /// already published/retired.
    pub fn supersede(&mut self, newer_source_version: u64) -> Result<(), DocumentChunkJobError> {
        if newer_source_version <= self.source_version {
            return Err(DocumentChunkJobError::SourceVersionNotNewer);
        }
        if matches!(
            self.status,
            DocumentChunkJobStatus::Ready | DocumentChunkJobStatus::Retired
        ) {
            return Err(DocumentChunkJobError::InvalidTransition);
        }
        self.status = DocumentChunkJobStatus::Superseded;
        self.lease = None;
        Ok(())
    }

    /// Returns the current lifecycle status.
    #[must_use]
    pub const fn status(&self) -> DocumentChunkJobStatus {
        self.status
    }

    /// Returns the number of acquired attempts.
    #[must_use]
    pub const fn attempt(&self) -> u32 {
        self.attempt
    }

    /// Returns the monotonic work checkpoint.
    #[must_use]
    pub const fn processed_units(&self) -> u64 {
        self.processed_units
    }

    /// Returns the prior generation retained while work is incomplete.
    #[must_use]
    pub const fn prior_ready_generation(&self) -> Option<u64> {
        self.prior_ready_generation
    }

    fn require_token(&self, lease: DocumentChunkLease) -> Result<(), DocumentChunkJobError> {
        if self.lease != Some(lease) {
            return Err(DocumentChunkJobError::StaleLease);
        }
        Ok(())
    }

    fn require_lease(
        &self,
        lease: DocumentChunkLease,
        now: u64,
    ) -> Result<(), DocumentChunkJobError> {
        self.require_token(lease)?;
        if now >= lease.expires_at {
            return Err(DocumentChunkJobError::LeaseExpired);
        }
        Ok(())
    }
}

/// Pure automatic-chunking lifecycle failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentChunkJobError {
    /// A durable identity used the reserved zero value.
    InvalidIdentity,
    /// Lease token or duration is invalid.
    InvalidLease,
    /// Another worker still owns the current lease.
    LeaseHeld,
    /// Supplied lease does not match the current fencing token/expiry.
    StaleLease,
    /// Current lease reached its exclusive expiry.
    LeaseExpired,
    /// Requested lifecycle edge is illegal.
    InvalidTransition,
    /// Progress regressed or exceeded the declared total.
    ProgressExceedsTotal,
    /// Maximum retry attempts have been consumed.
    RetryLimit,
    /// Publication replay did not match the committed digest.
    ConflictingPublication,
    /// Superseding source version was not strictly newer.
    SourceVersionNotNewer,
    /// Checked arithmetic overflowed.
    ArithmeticOverflow,
}

impl fmt::Display for DocumentChunkJobError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidIdentity => "invalid document chunk job identity",
            Self::InvalidLease => "invalid document chunk job lease",
            Self::LeaseHeld => "document chunk job lease is held",
            Self::StaleLease => "stale document chunk job lease",
            Self::LeaseExpired => "document chunk job lease expired",
            Self::InvalidTransition => "invalid document chunk job transition",
            Self::ProgressExceedsTotal => "document chunk job progress exceeds its total",
            Self::RetryLimit => "document chunk job retry limit reached",
            Self::ConflictingPublication => "conflicting document chunk publication",
            Self::SourceVersionNotNewer => "document chunk source version is not newer",
            Self::ArithmeticOverflow => "document chunk job arithmetic overflowed",
        })
    }
}

impl std::error::Error for DocumentChunkJobError {}
