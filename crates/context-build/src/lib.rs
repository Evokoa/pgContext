//! Pure lifecycle contracts for resumable pgContext generation builds.
//!
//! PostgreSQL persistence, WAL, files, clocks, and worker processes remain in
//! infrastructure adapters. This crate owns the deterministic job, lease,
//! validation, publication, pin, and retirement state machines.

use std::collections::BTreeSet;
use std::fmt::{Display, Formatter};

pub use context_core::{ConfigurationRevision, GenerationId, PointId, SourceVersion};

mod chunking;
mod document_chunk_job;
mod external;
mod findings;
mod lifecycle_types;
mod rowset;
mod token_chunking;

pub use chunking::{
    Chunk, ChunkError, ChunkProfile, MAX_CHUNK_CHARS, MAX_CHUNK_SOURCE_CHARS,
    MAX_CHUNKS_PER_DOCUMENT, chunk_document,
};
pub use document_chunk_job::{
    DocumentChunkJob, DocumentChunkJobError, DocumentChunkJobStatus, DocumentChunkLease,
    MAX_DOCUMENT_CHUNK_JOB_ATTEMPTS, MAX_DOCUMENT_CHUNK_LEASE_TICKS,
};
pub use external::{
    KmeansResult, MemoryBudget, SpillError, SpillRun, TrainingError, deterministic_kmeans,
    deterministic_metric_clusters, deterministic_metric_clusters_with_workers,
    deterministic_sample,
};
pub use findings::{FindingCode, FindingLocation, FindingSeverity, StructuralFinding};
pub use lifecycle_types::{ArtifactKind, BuildJobKind, build_contract_version};
pub use rowset::{OrderedRow, OrderedRowset, RowsetError};
pub use token_chunking::{
    ChunkIdentityContext, ChunkOccurrenceId, DocumentParser, MAX_CERTIFIED_PROFILE_OVERLAP_TOKENS,
    MAX_CONTEXT_PREFIX_TOKENS, MAX_STRUCTURE_DEPTH, MAX_STRUCTURE_SEGMENT_BYTES,
    MAX_TOKEN_CHUNK_DOCUMENT_BYTES, MAX_TOKEN_CHUNK_OUTPUT_BYTES, MAX_TOKENS_PER_CHUNK,
    MAX_TOKENS_PER_DOCUMENT, StructureKind, TokenChunk, TokenChunkError, TokenChunkProfile,
    TokenizerRevision, WordToken, bounded_unicode_word_context_prefix, chunk_document_tokens,
    chunk_document_tokens_with_identity, chunk_document_tokens_with_identity_and_checkpoint,
    unicode_word_token_count_up_to, unicode_word_tokens,
};

const MAX_PUBLICATION_ALIAS_BYTES: usize = 128;
const MAX_ARTIFACT_NAME_BYTES: usize = 255;

/// Durable lifecycle state for a resumable generation job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildJobStatus {
    /// Metadata exists but no worker holds a lease.
    Planned,
    /// A supervised worker holds the lease and advances source work.
    Running,
    /// The worker must stop at its next safe checkpoint.
    CancelRequested,
    /// The worker stopped without advancing past the cancellation checkpoint.
    Cancelled,
    /// Source work is complete and structural validation is pending.
    Validating,
    /// Validation passed and atomic publication is pending.
    Publishing,
    /// One generation was published successfully.
    Completed,
    /// The worker or validator recorded a recoverable failure.
    Failed,
    /// A nonterminal worker lease expired before completion.
    Abandoned,
}

impl BuildJobStatus {
    /// Parses the stable catalog representation.
    #[must_use]
    pub const fn from_catalog(value: &str) -> Option<Self> {
        match value.as_bytes() {
            b"planned" => Some(Self::Planned),
            b"running" => Some(Self::Running),
            b"cancel_requested" => Some(Self::CancelRequested),
            b"cancelled" => Some(Self::Cancelled),
            b"validating" => Some(Self::Validating),
            b"publishing" => Some(Self::Publishing),
            b"completed" => Some(Self::Completed),
            b"failed" => Some(Self::Failed),
            b"abandoned" => Some(Self::Abandoned),
            _ => None,
        }
    }

    /// Returns the stable catalog representation.
    #[must_use]
    pub const fn as_catalog(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Running => "running",
            Self::CancelRequested => "cancel_requested",
            Self::Cancelled => "cancelled",
            Self::Validating => "validating",
            Self::Publishing => "publishing",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Abandoned => "abandoned",
        }
    }

    /// Returns whether the shared supervised lifecycle permits `next`.
    #[must_use]
    pub const fn allows_transition(self, next: Self) -> bool {
        if self as u8 == next as u8 {
            return true;
        }
        matches!(
            (self, next),
            (
                Self::Planned | Self::Abandoned,
                Self::Running | Self::Validating
            ) | (Self::Planned, Self::Cancelled)
                | (
                    Self::Running,
                    Self::CancelRequested | Self::Validating | Self::Failed | Self::Abandoned
                )
                | (
                    Self::CancelRequested,
                    Self::Cancelled | Self::Failed | Self::Abandoned
                )
                | (
                    Self::Validating,
                    Self::CancelRequested | Self::Publishing | Self::Failed | Self::Abandoned
                )
                | (
                    Self::Publishing,
                    Self::Validating | Self::Completed | Self::Failed | Self::Abandoned
                )
                | (
                    Self::Failed | Self::Cancelled | Self::Abandoned,
                    Self::Planned
                )
        )
    }
}

/// Stable non-zero identity for a supervised worker instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct WorkerId(u64);

impl WorkerId {
    /// Creates a worker identity, rejecting the reserved zero value.
    #[must_use]
    pub const fn new(value: u64) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    /// Returns the numeric worker identity.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Logical-clock lease held by the current job attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobLease {
    worker: WorkerId,
    expires_at: u64,
}

impl JobLease {
    fn new(worker: WorkerId, now: u64, ttl: u64) -> Result<Self, BuildError> {
        if ttl == 0 {
            return Err(BuildError::ZeroLeaseDuration);
        }
        let expires_at = now.checked_add(ttl).ok_or(BuildError::ClockOverflow)?;
        Ok(Self { worker, expires_at })
    }

    /// Returns the owning worker.
    #[must_use]
    pub const fn worker(self) -> WorkerId {
        self.worker
    }

    /// Returns the exclusive logical-clock expiry instant.
    #[must_use]
    pub const fn expires_at(self) -> u64 {
        self.expires_at
    }

    /// Returns whether the lease has expired at `now`.
    #[must_use]
    pub const fn is_expired(self, now: u64) -> bool {
        now >= self.expires_at
    }
}

/// Monotonic source-work position persisted by an adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildCheckpoint {
    processed_units: u64,
    total_units: u64,
}

impl BuildCheckpoint {
    /// Creates a checkpoint bounded by its declared total.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::ProgressExceedsTotal`] when `processed_units`
    /// exceeds `total_units`.
    pub const fn new(processed_units: u64, total_units: u64) -> Result<Self, BuildError> {
        if processed_units > total_units {
            return Err(BuildError::ProgressExceedsTotal);
        }
        Ok(Self {
            processed_units,
            total_units,
        })
    }

    /// Returns completed source-work units.
    #[must_use]
    pub const fn processed_units(self) -> u64 {
        self.processed_units
    }

    /// Returns declared source-work units.
    #[must_use]
    pub const fn total_units(self) -> u64 {
        self.total_units
    }

    /// Returns whether all source work is checkpointed.
    #[must_use]
    pub const fn is_complete(self) -> bool {
        self.processed_units == self.total_units
    }

    fn checkpoint_to(self, processed_units: u64) -> Result<Self, BuildError> {
        if processed_units > self.total_units {
            return Err(BuildError::ProgressExceedsTotal);
        }
        if processed_units <= self.processed_units {
            return Ok(self);
        }
        Ok(Self {
            processed_units,
            total_units: self.total_units,
        })
    }
}

/// Fixed-width validation outcome safe to persist in a manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidationResult {
    passed: bool,
    finding_count: u32,
}

impl ValidationResult {
    /// Creates a passing validation result.
    #[must_use]
    pub const fn passed(finding_count: u32) -> Self {
        Self {
            passed: true,
            finding_count,
        }
    }

    /// Creates a failing validation result.
    #[must_use]
    pub const fn failed(finding_count: u32) -> Self {
        Self {
            passed: false,
            finding_count,
        }
    }

    /// Returns whether validation passed.
    #[must_use]
    pub const fn is_passed(self) -> bool {
        self.passed
    }

    /// Returns the bounded structural finding count.
    #[must_use]
    pub const fn finding_count(self) -> u32 {
        self.finding_count
    }
}

/// Pure state required to validate a durable job transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildJobState {
    kind: BuildJobKind,
    artifact_kind: ArtifactKind,
    status: BuildJobStatus,
    attempt: u32,
    checkpoint: BuildCheckpoint,
    lease: Option<JobLease>,
    validation: Option<ValidationResult>,
    published_generation: Option<GenerationId>,
}

impl BuildJobState {
    /// Creates a planned first attempt.
    #[must_use]
    pub const fn planned(
        kind: BuildJobKind,
        artifact_kind: ArtifactKind,
        total_units: u64,
    ) -> Self {
        Self {
            kind,
            artifact_kind,
            status: BuildJobStatus::Planned,
            attempt: 1,
            checkpoint: BuildCheckpoint {
                processed_units: 0,
                total_units,
            },
            lease: None,
            validation: None,
            published_generation: None,
        }
    }

    /// Returns the job operation.
    #[must_use]
    pub const fn kind(self) -> BuildJobKind {
        self.kind
    }

    /// Returns the output kind.
    #[must_use]
    pub const fn artifact_kind(self) -> ArtifactKind {
        self.artifact_kind
    }

    /// Returns lifecycle status.
    #[must_use]
    pub const fn status(self) -> BuildJobStatus {
        self.status
    }

    /// Returns the one-based attempt number.
    #[must_use]
    pub const fn attempt(self) -> u32 {
        self.attempt
    }

    /// Returns the monotonic checkpoint.
    #[must_use]
    pub const fn checkpoint(self) -> BuildCheckpoint {
        self.checkpoint
    }

    /// Returns the active worker lease.
    #[must_use]
    pub const fn lease(self) -> Option<JobLease> {
        self.lease
    }

    /// Returns the recorded validation outcome.
    #[must_use]
    pub const fn validation(self) -> Option<ValidationResult> {
        self.validation
    }

    /// Returns the published generation after successful completion.
    #[must_use]
    pub const fn published_generation(self) -> Option<GenerationId> {
        self.published_generation
    }

    /// Claims planned or retryable work with a bounded lease.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::InvalidTransition`] when the job is active or
    /// terminal, and lease errors for invalid logical-clock inputs.
    pub fn claim(mut self, worker: WorkerId, now: u64, ttl: u64) -> Result<Self, BuildError> {
        match self.status {
            BuildJobStatus::Planned => {}
            BuildJobStatus::Abandoned => {
                self.attempt = self
                    .attempt
                    .checked_add(1)
                    .ok_or(BuildError::AttemptOverflow)?;
            }
            _ => return Err(BuildError::InvalidTransition),
        }
        self.lease = Some(JobLease::new(worker, now, ttl)?);
        self.validation = None;
        self.published_generation = None;
        self.status = if self.checkpoint.is_complete() {
            BuildJobStatus::Validating
        } else {
            BuildJobStatus::Running
        };
        Ok(self)
    }

    /// Resets terminal retryable work to an ownerless planned attempt.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::InvalidTransition`] outside failed, cancelled, or
    /// abandoned states and [`BuildError::AttemptOverflow`] at the counter
    /// bound.
    pub fn retry(mut self) -> Result<Self, BuildError> {
        if !matches!(
            self.status,
            BuildJobStatus::Failed | BuildJobStatus::Cancelled | BuildJobStatus::Abandoned
        ) {
            return Err(BuildError::InvalidTransition);
        }
        self.attempt = self
            .attempt
            .checked_add(1)
            .ok_or(BuildError::AttemptOverflow)?;
        self.status = BuildJobStatus::Planned;
        self.lease = None;
        self.validation = None;
        self.published_generation = None;
        Ok(self)
    }

    /// Renews the current worker lease.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::LeaseOwnerMismatch`] for another worker and
    /// [`BuildError::LeaseExpired`] once the existing lease has expired.
    pub fn renew_lease(mut self, worker: WorkerId, now: u64, ttl: u64) -> Result<Self, BuildError> {
        let lease = self.lease.ok_or(BuildError::MissingLease)?;
        if lease.worker != worker {
            return Err(BuildError::LeaseOwnerMismatch);
        }
        if lease.is_expired(now) {
            return Err(BuildError::LeaseExpired);
        }
        self.lease = Some(JobLease::new(worker, now, ttl)?);
        Ok(self)
    }

    /// Marks active work abandoned after its lease expires.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::LeaseNotExpired`] while the owner still holds a
    /// valid lease, or [`BuildError::InvalidTransition`] outside active states.
    pub fn recover_expired_lease(mut self, now: u64) -> Result<Self, BuildError> {
        if !matches!(
            self.status,
            BuildJobStatus::Running
                | BuildJobStatus::CancelRequested
                | BuildJobStatus::Validating
                | BuildJobStatus::Publishing
        ) {
            return Err(BuildError::InvalidTransition);
        }
        let lease = self.lease.ok_or(BuildError::MissingLease)?;
        if !lease.is_expired(now) {
            return Err(BuildError::LeaseNotExpired);
        }
        self.status = BuildJobStatus::Abandoned;
        self.lease = None;
        Ok(self)
    }

    /// Records an absolute monotonic source checkpoint.
    ///
    /// Replaying the same or an older checkpoint is idempotent. A cancellation
    /// request stops at the existing checkpoint without applying new work.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::ProgressExceedsTotal`] for an out-of-range
    /// checkpoint and [`BuildError::InvalidTransition`] outside running work.
    pub fn checkpoint_to(mut self, processed_units: u64) -> Result<Self, BuildError> {
        match self.status {
            BuildJobStatus::CancelRequested => {
                self.status = BuildJobStatus::Cancelled;
                self.lease = None;
                Ok(self)
            }
            BuildJobStatus::Running => {
                self.checkpoint = self.checkpoint.checkpoint_to(processed_units)?;
                if self.checkpoint.is_complete() {
                    self.status = BuildJobStatus::Validating;
                }
                Ok(self)
            }
            _ => Err(BuildError::InvalidTransition),
        }
    }

    /// Requests cooperative cancellation.
    ///
    /// Repeating the request is idempotent.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::InvalidTransition`] outside planned, running, or
    /// validating work.
    pub const fn request_cancel(mut self) -> Result<Self, BuildError> {
        match self.status {
            BuildJobStatus::Planned => {
                self.status = BuildJobStatus::Cancelled;
                Ok(self)
            }
            BuildJobStatus::Running | BuildJobStatus::Validating => {
                self.status = BuildJobStatus::CancelRequested;
                Ok(self)
            }
            BuildJobStatus::CancelRequested => Ok(self),
            _ => Err(BuildError::InvalidTransition),
        }
    }

    /// Records structural validation and opens publication on success.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::InvalidTransition`] unless source work is fully
    /// checkpointed and validation is pending.
    pub const fn record_validation(
        mut self,
        validation: ValidationResult,
    ) -> Result<Self, BuildError> {
        if !matches!(self.status, BuildJobStatus::Validating) {
            return Err(BuildError::InvalidTransition);
        }
        self.validation = Some(validation);
        if validation.passed {
            self.status = BuildJobStatus::Publishing;
        } else {
            self.status = BuildJobStatus::Failed;
            self.lease = None;
        }
        Ok(self)
    }

    /// Returns publication to validation after the authoritative source
    /// revision advances before the atomic alias swap.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::InvalidTransition`] outside publication.
    pub const fn revalidate(mut self) -> Result<Self, BuildError> {
        if !matches!(self.status, BuildJobStatus::Publishing) {
            return Err(BuildError::InvalidTransition);
        }
        self.status = BuildJobStatus::Validating;
        self.validation = None;
        Ok(self)
    }

    /// Atomically records the published generation.
    ///
    /// Repeating publication for the same generation is idempotent.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::AlreadyPublishedDifferentGeneration`] when a
    /// completed job is replayed with another generation.
    pub fn publish(mut self, generation: GenerationId) -> Result<Self, BuildError> {
        match self.status {
            BuildJobStatus::Publishing => {
                self.published_generation = Some(generation);
                self.status = BuildJobStatus::Completed;
                self.lease = None;
                Ok(self)
            }
            BuildJobStatus::Completed if self.published_generation == Some(generation) => Ok(self),
            BuildJobStatus::Completed => Err(BuildError::AlreadyPublishedDifferentGeneration),
            _ => Err(BuildError::InvalidTransition),
        }
    }

    /// Records a recoverable failure in active work.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::InvalidTransition`] outside active states.
    pub const fn fail(mut self) -> Result<Self, BuildError> {
        if !matches!(
            self.status,
            BuildJobStatus::Running
                | BuildJobStatus::CancelRequested
                | BuildJobStatus::Validating
                | BuildJobStatus::Publishing
        ) {
            return Err(BuildError::InvalidTransition);
        }
        self.status = BuildJobStatus::Failed;
        self.lease = None;
        Ok(self)
    }
}

/// Validated publication alias switched atomically by an adapter.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PublicationAlias(String);

impl PublicationAlias {
    /// Parses a non-empty bounded alias.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::InvalidName`] for empty, surrounding-whitespace,
    /// control-character, or overlong aliases.
    pub fn new(value: impl Into<String>) -> Result<Self, BuildError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > MAX_PUBLICATION_ALIAS_BYTES
            || value.trim() != value
            || value.chars().any(char::is_control)
        {
            return Err(BuildError::InvalidName);
        }
        Ok(Self(value))
    }

    /// Returns the alias text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One checksummed item in a generation inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactDescriptor {
    kind: ArtifactKind,
    name: String,
    payload_bytes: u64,
    checksum: u64,
}

impl ArtifactDescriptor {
    /// Creates a named artifact descriptor.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::InvalidName`] for invalid inventory names.
    pub fn new(
        kind: ArtifactKind,
        name: impl Into<String>,
        payload_bytes: u64,
        checksum: u64,
    ) -> Result<Self, BuildError> {
        let name = name.into();
        if name.is_empty()
            || name.len() > MAX_ARTIFACT_NAME_BYTES
            || name.trim() != name
            || name.chars().any(char::is_control)
        {
            return Err(BuildError::InvalidName);
        }
        Ok(Self {
            kind,
            name,
            payload_bytes,
            checksum,
        })
    }

    /// Returns the artifact kind.
    #[must_use]
    pub const fn kind(&self) -> ArtifactKind {
        self.kind
    }

    /// Returns the inventory-local artifact name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns payload size in bytes.
    #[must_use]
    pub const fn payload_bytes(&self) -> u64 {
        self.payload_bytes
    }

    /// Returns the recorded checksum.
    #[must_use]
    pub const fn checksum(&self) -> u64 {
        self.checksum
    }
}

/// Publication and retirement state for an immutable generation manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationState {
    /// Inventory exists but validation has not completed.
    Staged,
    /// Inventory validation passed.
    Validated,
    /// The publication alias points at this generation.
    Published,
    /// Publication moved away, but readers still hold pins.
    Retiring,
    /// No new readers may attach and all pins are released.
    Retired,
    /// Validation failed and the inventory cannot be published.
    RebuildRequired,
}

/// Generic immutable generation inventory and publication state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationManifest {
    generation: GenerationId,
    source_version: SourceVersion,
    configuration: ConfigurationRevision,
    alias: PublicationAlias,
    artifacts: Vec<ArtifactDescriptor>,
    total_payload_bytes: u64,
    validation: Option<ValidationResult>,
    state: GenerationState,
    reader_pins: u32,
}

impl GenerationManifest {
    /// Creates a staged, non-empty, duplicate-free inventory.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::EmptyArtifactInventory`],
    /// [`BuildError::DuplicateArtifact`], or [`BuildError::SizeOverflow`] when
    /// the inventory cannot form a bounded manifest.
    pub fn staged(
        generation: GenerationId,
        source_version: SourceVersion,
        configuration: ConfigurationRevision,
        alias: PublicationAlias,
        artifacts: Vec<ArtifactDescriptor>,
    ) -> Result<Self, BuildError> {
        if artifacts.is_empty() {
            return Err(BuildError::EmptyArtifactInventory);
        }
        let mut identities = BTreeSet::new();
        let mut total_payload_bytes = 0_u64;
        for artifact in &artifacts {
            if !identities.insert((artifact.kind, artifact.name.as_str())) {
                return Err(BuildError::DuplicateArtifact);
            }
            total_payload_bytes = total_payload_bytes
                .checked_add(artifact.payload_bytes)
                .ok_or(BuildError::SizeOverflow)?;
        }
        Ok(Self {
            generation,
            source_version,
            configuration,
            alias,
            artifacts,
            total_payload_bytes,
            validation: None,
            state: GenerationState::Staged,
            reader_pins: 0,
        })
    }

    /// Returns the generation identity.
    #[must_use]
    pub const fn generation(&self) -> GenerationId {
        self.generation
    }

    /// Returns the authoritative source version used for the build.
    #[must_use]
    pub const fn source_version(&self) -> SourceVersion {
        self.source_version
    }

    /// Returns the immutable configuration revision.
    #[must_use]
    pub const fn configuration(&self) -> ConfigurationRevision {
        self.configuration
    }

    /// Returns the publication alias.
    #[must_use]
    pub const fn alias(&self) -> &PublicationAlias {
        &self.alias
    }

    /// Returns the immutable artifact inventory.
    #[must_use]
    pub fn artifacts(&self) -> &[ArtifactDescriptor] {
        &self.artifacts
    }

    /// Returns total payload bytes across the inventory.
    #[must_use]
    pub const fn total_payload_bytes(&self) -> u64 {
        self.total_payload_bytes
    }

    /// Returns validation result when recorded.
    #[must_use]
    pub const fn validation(&self) -> Option<ValidationResult> {
        self.validation
    }

    /// Returns lifecycle state.
    #[must_use]
    pub const fn state(&self) -> GenerationState {
        self.state
    }

    /// Returns active reader pins.
    #[must_use]
    pub const fn reader_pins(&self) -> u32 {
        self.reader_pins
    }

    /// Records structural validation.
    ///
    /// Replaying the same result is idempotent.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::ConflictingValidation`] when a different result
    /// was already recorded, or [`BuildError::InvalidTransition`] after
    /// publication begins.
    pub fn record_validation(mut self, validation: ValidationResult) -> Result<Self, BuildError> {
        match (self.state, self.validation) {
            (GenerationState::Staged, None) => {
                self.validation = Some(validation);
                self.state = if validation.passed {
                    GenerationState::Validated
                } else {
                    GenerationState::RebuildRequired
                };
                Ok(self)
            }
            (GenerationState::Validated | GenerationState::RebuildRequired, Some(existing))
                if existing == validation =>
            {
                Ok(self)
            }
            (GenerationState::Validated | GenerationState::RebuildRequired, Some(_)) => {
                Err(BuildError::ConflictingValidation)
            }
            _ => Err(BuildError::InvalidTransition),
        }
    }

    /// Publishes a validated manifest.
    ///
    /// Repeating publication is idempotent.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::ValidationFailed`] for failed validation and
    /// [`BuildError::InvalidTransition`] before validation or after retirement.
    pub fn publish(mut self) -> Result<Self, BuildError> {
        match self.state {
            GenerationState::Validated => {
                self.state = GenerationState::Published;
                Ok(self)
            }
            GenerationState::Published => Ok(self),
            GenerationState::RebuildRequired => Err(BuildError::ValidationFailed),
            _ => Err(BuildError::InvalidTransition),
        }
    }

    /// Acquires one bounded reader pin on a published generation.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::InvalidTransition`] when new readers are closed
    /// or [`BuildError::ReaderPinOverflow`] at the counter bound.
    pub fn pin(mut self) -> Result<Self, BuildError> {
        if !matches!(self.state, GenerationState::Published) {
            return Err(BuildError::InvalidTransition);
        }
        self.reader_pins = match self.reader_pins.checked_add(1) {
            Some(value) => value,
            None => return Err(BuildError::ReaderPinOverflow),
        };
        Ok(self)
    }

    /// Releases one reader pin and completes pending retirement at zero.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::ReaderPinUnderflow`] when no reader is pinned.
    pub fn unpin(mut self) -> Result<Self, BuildError> {
        if self.reader_pins == 0 {
            return Err(BuildError::ReaderPinUnderflow);
        }
        self.reader_pins -= 1;
        if self.reader_pins == 0 && matches!(self.state, GenerationState::Retiring) {
            self.state = GenerationState::Retired;
        }
        Ok(self)
    }

    /// Closes publication and starts or completes retirement.
    ///
    /// Repeating retirement is idempotent.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError::InvalidTransition`] before publication.
    pub fn retire(mut self) -> Result<Self, BuildError> {
        match self.state {
            GenerationState::Published => {
                self.state = if self.reader_pins == 0 {
                    GenerationState::Retired
                } else {
                    GenerationState::Retiring
                };
                Ok(self)
            }
            GenerationState::Retiring | GenerationState::Retired => Ok(self),
            _ => Err(BuildError::InvalidTransition),
        }
    }
}

/// Rejected shared build or generation operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildError {
    /// A typed identity used the reserved zero value.
    ZeroIdentity,
    /// A checkpoint exceeded its declared total.
    ProgressExceedsTotal,
    /// The requested lifecycle transition is not legal.
    InvalidTransition,
    /// A logical lease duration was zero.
    ZeroLeaseDuration,
    /// Lease expiry overflowed the logical clock.
    ClockOverflow,
    /// The attempt counter overflowed.
    AttemptOverflow,
    /// The job has no active lease.
    MissingLease,
    /// Another worker owns the active lease.
    LeaseOwnerMismatch,
    /// The current lease already expired.
    LeaseExpired,
    /// Recovery was requested while the lease remained valid.
    LeaseNotExpired,
    /// A completed job was replayed with another generation.
    AlreadyPublishedDifferentGeneration,
    /// A public alias or artifact name is invalid.
    InvalidName,
    /// A generation inventory contains no artifacts.
    EmptyArtifactInventory,
    /// A generation inventory repeats an artifact identity.
    DuplicateArtifact,
    /// Aggregate payload size overflowed.
    SizeOverflow,
    /// A different validation outcome was already recorded.
    ConflictingValidation,
    /// Failed validation prevents publication.
    ValidationFailed,
    /// Reader pin count overflowed.
    ReaderPinOverflow,
    /// Reader pin release was requested at zero.
    ReaderPinUnderflow,
}

impl Display for BuildError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::ZeroIdentity => "identity must be non-zero",
            Self::ProgressExceedsTotal => "build progress exceeds declared total",
            Self::InvalidTransition => "invalid build lifecycle transition",
            Self::ZeroLeaseDuration => "job lease duration must be non-zero",
            Self::ClockOverflow => "job lease logical clock overflow",
            Self::AttemptOverflow => "build attempt counter overflow",
            Self::MissingLease => "build job has no active lease",
            Self::LeaseOwnerMismatch => "build job lease belongs to another worker",
            Self::LeaseExpired => "build job lease has expired",
            Self::LeaseNotExpired => "build job lease has not expired",
            Self::AlreadyPublishedDifferentGeneration => {
                "build job already published a different generation"
            }
            Self::InvalidName => "generation alias or artifact name is invalid",
            Self::EmptyArtifactInventory => "generation artifact inventory must not be empty",
            Self::DuplicateArtifact => "generation artifact inventory contains a duplicate",
            Self::SizeOverflow => "generation payload size overflow",
            Self::ConflictingValidation => {
                "generation validation result conflicts with prior state"
            }
            Self::ValidationFailed => "failed generation validation prevents publication",
            Self::ReaderPinOverflow => "generation reader pin count overflow",
            Self::ReaderPinUnderflow => "generation reader pin count is already zero",
        })
    }
}
#[cfg(test)]
mod lib_tests;
