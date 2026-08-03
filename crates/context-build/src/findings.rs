//! Stable, redacted structural findings shared by artifact verifiers.

/// Severity of a structural verification finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FindingSeverity {
    /// Informational metadata that does not affect correctness.
    Info,
    /// Structural drift that should be investigated.
    Warning,
    /// A violated invariant requiring rebuild or repair.
    Error,
}

/// Stable machine-readable structural invariant code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FindingCode {
    /// Persisted format version is unsupported.
    UnsupportedFormat,
    /// Artifact size does not match its manifest.
    SizeMismatch,
    /// Artifact checksum does not match its manifest.
    ChecksumMismatch,
    /// A required generation reference is missing.
    MissingGeneration,
    /// The inventory repeats an identity that must be unique.
    DuplicateIdentity,
    /// Reader pins disagree with publication or retirement state.
    InvalidReaderPin,
    /// The verification budget ended before a conclusion was possible.
    BudgetExhausted,
}

/// Bounded structural location with no source key, payload, vector, or path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FindingLocation {
    generation: Option<u64>,
    artifact: Option<u64>,
    offset: Option<u64>,
}

impl FindingLocation {
    /// Identifies one artifact inside a generation.
    #[must_use]
    pub const fn artifact(generation: u64, artifact: u64) -> Self {
        Self {
            generation: Some(generation),
            artifact: Some(artifact),
            offset: None,
        }
    }

    /// Adds a format-local numeric offset.
    #[must_use]
    pub const fn with_offset(mut self, offset: u64) -> Self {
        self.offset = Some(offset);
        self
    }

    /// Returns the generation identity, when relevant.
    #[must_use]
    pub const fn generation(self) -> Option<u64> {
        self.generation
    }

    /// Returns the artifact identity, when relevant.
    #[must_use]
    pub const fn artifact_id(self) -> Option<u64> {
        self.artifact
    }

    /// Returns the format-local numeric offset, when relevant.
    #[must_use]
    pub const fn offset(self) -> Option<u64> {
        self.offset
    }
}

/// One redacted structural finding suitable for SQL and certification output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructuralFinding {
    severity: FindingSeverity,
    code: FindingCode,
    location: FindingLocation,
    invariant: String,
    guidance: String,
}

impl StructuralFinding {
    /// Creates a finding whose public text is selected entirely by its typed
    /// code. Callers can supply only bounded numeric locations, so source keys,
    /// tenant values, paths, vectors, and secrets cannot enter diagnostics.
    #[must_use]
    pub fn new(code: FindingCode, location: FindingLocation) -> Self {
        let (severity, invariant, guidance) = finding_text(code);
        Self {
            severity,
            code,
            location,
            invariant: invariant.to_owned(),
            guidance: guidance.to_owned(),
        }
    }

    /// Returns severity.
    #[must_use]
    pub const fn severity(&self) -> FindingSeverity {
        self.severity
    }

    /// Returns stable code.
    #[must_use]
    pub const fn code(&self) -> FindingCode {
        self.code
    }

    /// Returns redacted location.
    #[must_use]
    pub const fn location(&self) -> FindingLocation {
        self.location
    }

    /// Returns the violated invariant.
    #[must_use]
    pub fn invariant(&self) -> &str {
        &self.invariant
    }

    /// Returns operator guidance.
    #[must_use]
    pub fn guidance(&self) -> &str {
        &self.guidance
    }
}

const fn finding_text(code: FindingCode) -> (FindingSeverity, &'static str, &'static str) {
    match code {
        FindingCode::UnsupportedFormat => (
            FindingSeverity::Error,
            "artifact format version is unsupported",
            "rebuild the derived generation with a supported format",
        ),
        FindingCode::SizeMismatch => (
            FindingSeverity::Error,
            "artifact size differs from its manifest",
            "discard and rebuild the derived generation",
        ),
        FindingCode::ChecksumMismatch => (
            FindingSeverity::Error,
            "artifact checksum differs from its manifest",
            "discard and rebuild the derived generation",
        ),
        FindingCode::MissingGeneration => (
            FindingSeverity::Error,
            "required generation reference is missing",
            "repair the catalog reference or rebuild the derived generation",
        ),
        FindingCode::DuplicateIdentity => (
            FindingSeverity::Error,
            "artifact inventory contains a duplicate identity",
            "reject publication and rebuild the derived generation",
        ),
        FindingCode::InvalidReaderPin => (
            FindingSeverity::Warning,
            "reader pin disagrees with generation lifecycle state",
            "reconcile stale pins before retiring the generation",
        ),
        FindingCode::BudgetExhausted => (
            FindingSeverity::Warning,
            "verification ended at its configured work budget",
            "raise the bounded verification budget and retry",
        ),
    }
}
