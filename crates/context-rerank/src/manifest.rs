//! Immutable model-manifest validation and artifact admission.

use std::{
    collections::BTreeSet,
    fs::File,
    io::Read,
    path::{Component, Path, PathBuf},
};

use context_query::{MAX_RERANK_CANDIDATES, MAX_RERANK_REQUEST_BYTES, RerankModelName};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Current worker-manifest schema.
pub const WORKER_MANIFEST_VERSION: u32 = 1;
/// Maximum serialized manifest size admitted before JSON parsing.
pub const MAX_WORKER_MANIFEST_BYTES: usize = 64 * 1024;
/// Maximum operator-provided artifact size.
pub const MAX_WORKER_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;
/// Maximum tokenizer revision bytes.
pub const MAX_TOKENIZER_REVISION_BYTES: usize = 128;
/// Maximum supported-platform entries.
pub const MAX_SUPPORTED_PLATFORMS: usize = 8;
/// Maximum token count accepted by the private fixture adapter.
pub const MAX_WORKER_TOKENS: usize = 65_536;
/// Maximum worker deadline admitted by a manifest.
pub const MAX_WORKER_ELAPSED_MICROS: u64 = 60_000_000;
/// Maximum retry attempts after the original worker call.
pub const MAX_WORKER_RETRIES: usize = 8;
/// Maximum consecutive failures admitted by the circuit breaker.
pub const MAX_WORKER_BREAKER_FAILURES: usize = 64;
/// Tokenizer revision implemented by the private `linear_pair_v1` adapter.
pub const LINEAR_PAIR_V1_TOKENIZER_REVISION: &str = "ascii_tokens_v1";

/// Stable worker-manifest or artifact verification failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManifestError {
    /// The manifest is malformed, unknown, or outside a declared bound.
    InvalidManifest,
    /// The configured platform does not include this worker binary.
    UnsupportedPlatform,
    /// The artifact could not be opened or read safely.
    ArtifactUnavailable,
    /// The artifact length differs from the immutable manifest.
    ArtifactLengthMismatch,
    /// The artifact digest differs from the immutable manifest.
    ArtifactDigestMismatch,
}

impl ManifestError {
    /// Returns a content-free diagnostic suitable for logs.
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::InvalidManifest => "invalid_manifest",
            Self::UnsupportedPlatform => "unsupported_platform",
            Self::ArtifactUnavailable => "artifact_unavailable",
            Self::ArtifactLengthMismatch => "artifact_length_mismatch",
            Self::ArtifactDigestMismatch => "artifact_digest_mismatch",
        }
    }
}

impl core::fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.stable_name())
    }
}

impl std::error::Error for ManifestError {}

/// Private adapter selected by an immutable manifest.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerAdapterKind {
    /// Pure-Rust bounded fixture used for P12 certification.
    LinearPairV1,
}

/// Meaning and direction of scores returned by the adapter.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerScoreContract {
    /// Finite relevance in `[0, 1]`, with larger values ranked first.
    HigherIsBetterUnitInterval,
}

/// Validated request contract accepted by a worker adapter.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerInputContract {
    /// The version-three provider-neutral rerank envelope.
    RerankEnvelopeV3,
}

/// Validated response contract emitted by a worker adapter.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerOutputContract {
    /// The version-three provider-neutral rerank response.
    RerankResponseV3,
}

/// Bounded operational-failure frame emitted by the persistent worker.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerFailureContract {
    /// Object with stable `error` and SQL-compatible `failure_reason` fields.
    RerankFailureV1,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum DistributionPolicy {
    OperatorProvidedOnly,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    schema_version: u32,
    adapter: WorkerAdapterKind,
    model: String,
    model_revision: u64,
    artifact_path: String,
    artifact_bytes: u64,
    artifact_sha256: String,
    tokenizer_revision: String,
    input_contract: WorkerInputContract,
    output_contract: WorkerOutputContract,
    failure_contract: WorkerFailureContract,
    score_contract: WorkerScoreContract,
    max_request_bytes: usize,
    max_candidates: usize,
    max_query_tokens: usize,
    max_document_tokens: usize,
    max_elapsed_micros: u64,
    max_retries: usize,
    breaker_failure_threshold: usize,
    breaker_cooldown_micros: u64,
    supported_platforms: Vec<String>,
    license_spdx: String,
    license_url: String,
    distribution: DistributionPolicy,
}

/// Validated immutable worker configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerManifest {
    adapter: WorkerAdapterKind,
    model: RerankModelName,
    model_revision: u64,
    artifact_path: PathBuf,
    artifact_bytes: usize,
    artifact_sha256: [u8; 32],
    tokenizer_revision: String,
    input_contract: WorkerInputContract,
    output_contract: WorkerOutputContract,
    failure_contract: WorkerFailureContract,
    score_contract: WorkerScoreContract,
    max_request_bytes: usize,
    max_candidates: usize,
    max_query_tokens: usize,
    max_document_tokens: usize,
    max_elapsed_micros: u64,
    max_retries: usize,
    breaker_failure_threshold: usize,
    breaker_cooldown_micros: u64,
    supported_platforms: Vec<String>,
    license_spdx: String,
    license_url: String,
}

impl WorkerManifest {
    /// Reads and validates one bounded manifest file.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::InvalidManifest`] when the path is unavailable,
    /// is not a regular file, exceeds the manifest byte ceiling, or its JSON is
    /// invalid.
    pub fn load(path: &Path) -> Result<Self, ManifestError> {
        let mut file = File::open(path).map_err(|_| ManifestError::InvalidManifest)?;
        let metadata = file
            .metadata()
            .map_err(|_| ManifestError::InvalidManifest)?;
        let length = usize::try_from(metadata.len()).map_err(|_| ManifestError::InvalidManifest)?;
        if !metadata.is_file() || length > MAX_WORKER_MANIFEST_BYTES {
            return Err(ManifestError::InvalidManifest);
        }
        let limit = u64::try_from(MAX_WORKER_MANIFEST_BYTES)
            .map_err(|_| ManifestError::InvalidManifest)?
            .checked_add(1)
            .ok_or(ManifestError::InvalidManifest)?;
        let mut json = String::with_capacity(length);
        file.by_ref()
            .take(limit)
            .read_to_string(&mut json)
            .map_err(|_| ManifestError::InvalidManifest)?;
        if json.len() != length {
            return Err(ManifestError::InvalidManifest);
        }
        Self::from_json(&json)
    }

    /// Parses and validates one bounded JSON manifest.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::InvalidManifest`] for malformed JSON, unknown
    /// fields, unsupported schema values, invalid identities, or bounds.
    pub fn from_json(json: &str) -> Result<Self, ManifestError> {
        if json.len() > MAX_WORKER_MANIFEST_BYTES {
            return Err(ManifestError::InvalidManifest);
        }
        let raw: RawManifest =
            serde_json::from_str(json).map_err(|_| ManifestError::InvalidManifest)?;
        if raw.schema_version != WORKER_MANIFEST_VERSION
            || raw.model_revision == 0
            || raw.max_candidates == 0
            || raw.max_candidates > MAX_RERANK_CANDIDATES
            || raw.max_request_bytes == 0
            || raw.max_request_bytes > MAX_RERANK_REQUEST_BYTES
            || raw.max_query_tokens == 0
            || raw.max_query_tokens > MAX_WORKER_TOKENS
            || raw.max_document_tokens == 0
            || raw.max_document_tokens > MAX_WORKER_TOKENS
            || raw.max_elapsed_micros == 0
            || raw.max_elapsed_micros > MAX_WORKER_ELAPSED_MICROS
            || raw.max_retries > MAX_WORKER_RETRIES
            || raw.breaker_failure_threshold == 0
            || raw.breaker_failure_threshold > MAX_WORKER_BREAKER_FAILURES
            || raw.breaker_cooldown_micros == 0
            || raw.breaker_cooldown_micros > MAX_WORKER_ELAPSED_MICROS
        {
            return Err(ManifestError::InvalidManifest);
        }
        let model = RerankModelName::new(raw.model).map_err(|_| ManifestError::InvalidManifest)?;
        let artifact_bytes =
            usize::try_from(raw.artifact_bytes).map_err(|_| ManifestError::InvalidManifest)?;
        if artifact_bytes == 0 || artifact_bytes > MAX_WORKER_ARTIFACT_BYTES {
            return Err(ManifestError::InvalidManifest);
        }
        let artifact_path = validate_artifact_path(&raw.artifact_path)?;
        let artifact_sha256 = decode_digest(&raw.artifact_sha256)?;
        validate_bounded_text(&raw.tokenizer_revision, MAX_TOKENIZER_REVISION_BYTES)?;
        validate_supported_platforms(&raw.supported_platforms)?;
        validate_spdx(&raw.license_spdx)?;
        validate_license_url(&raw.license_url)?;
        let _distribution = raw.distribution;

        Ok(Self {
            adapter: raw.adapter,
            model,
            model_revision: raw.model_revision,
            artifact_path,
            artifact_bytes,
            artifact_sha256,
            tokenizer_revision: raw.tokenizer_revision,
            input_contract: raw.input_contract,
            output_contract: raw.output_contract,
            failure_contract: raw.failure_contract,
            score_contract: raw.score_contract,
            max_request_bytes: raw.max_request_bytes,
            max_candidates: raw.max_candidates,
            max_query_tokens: raw.max_query_tokens,
            max_document_tokens: raw.max_document_tokens,
            max_elapsed_micros: raw.max_elapsed_micros,
            max_retries: raw.max_retries,
            breaker_failure_threshold: raw.breaker_failure_threshold,
            breaker_cooldown_micros: raw.breaker_cooldown_micros,
            supported_platforms: raw.supported_platforms,
            license_spdx: raw.license_spdx,
            license_url: raw.license_url,
        })
    }

    /// Returns the selected private adapter.
    #[must_use]
    pub const fn adapter(&self) -> WorkerAdapterKind {
        self.adapter
    }

    /// Returns the immutable model name.
    #[must_use]
    pub const fn model(&self) -> &RerankModelName {
        &self.model
    }

    /// Returns the immutable model revision.
    #[must_use]
    pub const fn model_revision(&self) -> u64 {
        self.model_revision
    }

    /// Returns the expected artifact SHA-256 as lowercase hexadecimal.
    #[must_use]
    pub fn artifact_sha256(&self) -> String {
        encode_digest(&self.artifact_sha256)
    }

    /// Returns the tokenizer revision.
    #[must_use]
    pub fn tokenizer_revision(&self) -> &str {
        &self.tokenizer_revision
    }

    /// Returns the validated input contract.
    #[must_use]
    pub const fn input_contract(&self) -> WorkerInputContract {
        self.input_contract
    }

    /// Returns the validated output contract.
    #[must_use]
    pub const fn output_contract(&self) -> WorkerOutputContract {
        self.output_contract
    }

    /// Returns the persistent worker's operational-failure frame contract.
    #[must_use]
    pub const fn failure_contract(&self) -> WorkerFailureContract {
        self.failure_contract
    }

    /// Returns the score contract.
    #[must_use]
    pub const fn score_contract(&self) -> WorkerScoreContract {
        self.score_contract
    }

    /// Returns the maximum validated request allocation admitted by the adapter.
    #[must_use]
    pub const fn max_request_bytes(&self) -> usize {
        self.max_request_bytes
    }

    /// Returns the maximum candidates per request.
    #[must_use]
    pub const fn max_candidates(&self) -> usize {
        self.max_candidates
    }

    /// Returns the maximum query tokens.
    #[must_use]
    pub const fn max_query_tokens(&self) -> usize {
        self.max_query_tokens
    }

    /// Returns the maximum document tokens.
    #[must_use]
    pub const fn max_document_tokens(&self) -> usize {
        self.max_document_tokens
    }

    /// Returns the maximum elapsed time per request.
    #[must_use]
    pub const fn max_elapsed_micros(&self) -> u64 {
        self.max_elapsed_micros
    }

    /// Returns the allowed retries after the first attempt.
    #[must_use]
    pub const fn max_retries(&self) -> usize {
        self.max_retries
    }

    /// Returns the consecutive-failure threshold that opens the circuit.
    #[must_use]
    pub const fn breaker_failure_threshold(&self) -> usize {
        self.breaker_failure_threshold
    }

    /// Returns the circuit-breaker cooldown in microseconds.
    #[must_use]
    pub const fn breaker_cooldown_micros(&self) -> u64 {
        self.breaker_cooldown_micros
    }

    /// Returns the declared SPDX license identifier.
    #[must_use]
    pub fn license_spdx(&self) -> &str {
        &self.license_spdx
    }

    /// Returns the declared HTTPS license URL.
    #[must_use]
    pub fn license_url(&self) -> &str {
        &self.license_url
    }
}

/// Digest-verified artifact bytes admitted under a [`WorkerManifest`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedArtifact {
    bytes: Vec<u8>,
}

impl VerifiedArtifact {
    /// Opens, bounds, and digest-verifies the manifest's operator artifact.
    ///
    /// # Errors
    ///
    /// Returns a stable [`ManifestError`] when the platform is unsupported,
    /// the path cannot be confined beneath `manifest_directory`, or the bytes
    /// differ from the immutable length/digest.
    pub fn load(
        manifest: &WorkerManifest,
        manifest_directory: &Path,
    ) -> Result<Self, ManifestError> {
        if current_platform_id() == "unsupported"
            || !manifest
                .supported_platforms
                .iter()
                .any(|platform| platform == current_platform_id())
        {
            return Err(ManifestError::UnsupportedPlatform);
        }
        let root = manifest_directory
            .canonicalize()
            .map_err(|_| ManifestError::ArtifactUnavailable)?;
        if !root.is_dir() {
            return Err(ManifestError::ArtifactUnavailable);
        }
        let path = root.join(&manifest.artifact_path);
        let canonical = path
            .canonicalize()
            .map_err(|_| ManifestError::ArtifactUnavailable)?;
        if !canonical.starts_with(&root) {
            return Err(ManifestError::ArtifactUnavailable);
        }
        let mut file = File::open(&canonical).map_err(|_| ManifestError::ArtifactUnavailable)?;
        let metadata = file
            .metadata()
            .map_err(|_| ManifestError::ArtifactUnavailable)?;
        if !metadata.is_file() {
            return Err(ManifestError::ArtifactUnavailable);
        }
        let expected =
            u64::try_from(manifest.artifact_bytes).map_err(|_| ManifestError::InvalidManifest)?;
        if metadata.len() != expected {
            return Err(ManifestError::ArtifactLengthMismatch);
        }
        let read_limit = expected
            .checked_add(1)
            .ok_or(ManifestError::InvalidManifest)?;
        let mut bytes = Vec::with_capacity(manifest.artifact_bytes);
        file.by_ref()
            .take(read_limit)
            .read_to_end(&mut bytes)
            .map_err(|_| ManifestError::ArtifactUnavailable)?;
        if bytes.len() != manifest.artifact_bytes {
            return Err(ManifestError::ArtifactLengthMismatch);
        }
        let actual = Sha256::digest(&bytes);
        if actual.as_slice() != manifest.artifact_sha256 {
            return Err(ManifestError::ArtifactDigestMismatch);
        }
        Ok(Self { bytes })
    }

    /// Returns the verified immutable bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Returns this binary's stable manifest platform identity.
#[must_use]
pub const fn current_platform_id() -> &'static str {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "darwin-aarch64"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "darwin-x86_64"
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        "linux-aarch64"
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "linux-x86_64"
    }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64")
    )))]
    {
        "unsupported"
    }
}

fn validate_artifact_path(raw: &str) -> Result<PathBuf, ManifestError> {
    if raw.is_empty() || raw.len() > 512 || raw.chars().any(char::is_control) {
        return Err(ManifestError::InvalidManifest);
    }
    let path = Path::new(raw);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ManifestError::InvalidManifest);
    }
    Ok(path.to_path_buf())
}

fn decode_digest(raw: &str) -> Result<[u8; 32], ManifestError> {
    if raw.len() != 64 || !raw.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ManifestError::InvalidManifest);
    }
    let mut digest = [0_u8; 32];
    for (index, pair) in raw.as_bytes().chunks_exact(2).enumerate() {
        let high = decode_nibble(pair[0])?;
        let low = decode_nibble(pair[1])?;
        digest[index] = (high << 4) | low;
    }
    if encode_digest(&digest) != raw {
        return Err(ManifestError::InvalidManifest);
    }
    Ok(digest)
}

fn decode_nibble(byte: u8) -> Result<u8, ManifestError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(ManifestError::InvalidManifest),
    }
}

fn encode_digest(digest: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(64);
    for byte in digest {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn validate_bounded_text(value: &str, maximum: usize) -> Result<(), ManifestError> {
    if value.len() > maximum || value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(ManifestError::InvalidManifest);
    }
    Ok(())
}

fn validate_supported_platforms(platforms: &[String]) -> Result<(), ManifestError> {
    if platforms.is_empty() || platforms.len() > MAX_SUPPORTED_PLATFORMS {
        return Err(ManifestError::InvalidManifest);
    }
    let mut unique = BTreeSet::new();
    for platform in platforms {
        validate_bounded_text(platform, 64)?;
        if !matches!(
            platform.as_str(),
            "darwin-aarch64" | "darwin-x86_64" | "linux-aarch64" | "linux-x86_64"
        ) || !unique.insert(platform.as_str())
        {
            return Err(ManifestError::InvalidManifest);
        }
    }
    Ok(())
}

fn validate_spdx(spdx: &str) -> Result<(), ManifestError> {
    validate_bounded_text(spdx, 128)?;
    if !spdx.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'+' | b'(' | b')' | b' ')
    }) {
        return Err(ManifestError::InvalidManifest);
    }
    Ok(())
}

fn validate_license_url(url: &str) -> Result<(), ManifestError> {
    if url.len() > 2_048 || !url.starts_with("https://") || url.chars().any(char::is_control) {
        return Err(ManifestError::InvalidManifest);
    }
    Ok(())
}
