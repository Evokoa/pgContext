//! Private pure-Rust fixture adapter selected for P12 certification.

use std::{
    collections::BTreeSet,
    mem::size_of,
    sync::atomic::{AtomicBool, Ordering},
};

use context_query::RerankRequest;

use crate::{
    BackendResult, LINEAR_PAIR_V1_TOKENIZER_REVISION, RerankBackend, RerankBackendError,
    VerifiedArtifact, WireRerankResponse, WireRerankScore, WorkerAdapterKind,
    WorkerFailureContract, WorkerInputContract, WorkerManifest, WorkerOutputContract,
    WorkerScoreContract,
};

const ARTIFACT_MAGIC: &[u8; 8] = b"PGLPAIR1";
const WEIGHT_COUNT: usize = 4;
const ARTIFACT_BYTES: usize = ARTIFACT_MAGIC.len() + WEIGHT_COUNT * size_of::<f64>();
const MAX_ABSOLUTE_WEIGHT: f64 = 64.0;

/// Why a verified artifact cannot construct the selected fixture adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinearPairV1Error {
    /// The verified bytes do not implement the frozen `linear_pair_v1` format.
    InvalidArtifact,
}

impl core::fmt::Display for LinearPairV1Error {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("invalid_linear_pair_v1_artifact")
    }
}

impl std::error::Error for LinearPairV1Error {}

/// Digest-verified, operator-provided `linear_pair_v1` fixture backend.
///
/// The adapter is deliberately private and small. It proves the complete
/// provider-neutral worker contract without claiming compatibility with a
/// transformer runtime. Its artifact contains four little-endian `f64`
/// weights after the eight-byte `PGLPAIR1` magic: bias, unique-token overlap,
/// exact query phrase, and document brevity.
#[derive(Clone, Debug)]
pub struct LinearPairV1 {
    model: String,
    model_revision: u64,
    max_candidates: usize,
    max_query_tokens: usize,
    max_document_tokens: usize,
    max_elapsed_micros: u64,
    weights: [f64; WEIGHT_COUNT],
}

impl LinearPairV1 {
    /// Constructs the adapter only from a separately verified artifact.
    ///
    /// # Errors
    ///
    /// Returns [`LinearPairV1Error::InvalidArtifact`] when the selected
    /// manifest contract or the frozen binary weight format is incompatible.
    pub fn load(
        manifest: &WorkerManifest,
        artifact: VerifiedArtifact,
    ) -> Result<Self, LinearPairV1Error> {
        if manifest.adapter() != WorkerAdapterKind::LinearPairV1
            || manifest.input_contract() != WorkerInputContract::RerankEnvelopeV3
            || manifest.output_contract() != WorkerOutputContract::RerankResponseV3
            || manifest.failure_contract() != WorkerFailureContract::RerankFailureV1
            || manifest.score_contract() != WorkerScoreContract::HigherIsBetterUnitInterval
            || manifest.tokenizer_revision() != LINEAR_PAIR_V1_TOKENIZER_REVISION
            || manifest.max_request_bytes() != context_query::MAX_RERANK_REQUEST_BYTES
        {
            return Err(LinearPairV1Error::InvalidArtifact);
        }
        let bytes = artifact.bytes();
        if bytes.len() != ARTIFACT_BYTES || &bytes[..ARTIFACT_MAGIC.len()] != ARTIFACT_MAGIC {
            return Err(LinearPairV1Error::InvalidArtifact);
        }
        let mut weights = [0.0; WEIGHT_COUNT];
        for (index, chunk) in bytes[ARTIFACT_MAGIC.len()..]
            .chunks_exact(size_of::<f64>())
            .enumerate()
        {
            let raw: [u8; size_of::<f64>()] = chunk
                .try_into()
                .map_err(|_| LinearPairV1Error::InvalidArtifact)?;
            let weight = f64::from_le_bytes(raw);
            if !weight.is_finite() || weight.abs() > MAX_ABSOLUTE_WEIGHT {
                return Err(LinearPairV1Error::InvalidArtifact);
            }
            weights[index] = weight;
        }
        Ok(Self {
            model: manifest.model().as_str().to_owned(),
            model_revision: manifest.model_revision(),
            max_candidates: manifest.max_candidates(),
            max_query_tokens: manifest.max_query_tokens(),
            max_document_tokens: manifest.max_document_tokens(),
            max_elapsed_micros: manifest.max_elapsed_micros(),
            weights,
        })
    }

    /// Returns the manifest's per-request deadline ceiling.
    #[must_use]
    pub const fn max_elapsed_micros(&self) -> u64 {
        self.max_elapsed_micros
    }

    pub(crate) fn score_with_cancel(
        &self,
        request: &RerankRequest,
        shutdown: &AtomicBool,
        attempt_cancelled: &AtomicBool,
    ) -> BackendResult<WireRerankResponse> {
        if request.model().as_str() != self.model
            || request.model_revision() != self.model_revision
            || request.candidates().len() > self.max_candidates
        {
            return Err(invalid_work());
        }
        let query = QueryFeatures::new(
            request.query().as_str(),
            self.max_query_tokens,
            shutdown,
            attempt_cancelled,
        )?;
        let mut scores = Vec::with_capacity(request.candidates().len());
        for candidate in request.candidates() {
            if cancelled(shutdown, attempt_cancelled) {
                return Err(RerankBackendError::Cancelled);
            }
            scores.push(WireRerankScore {
                occurrence_id: candidate.occurrence_id().get(),
                score: self.score_candidate(
                    &query,
                    candidate.text(),
                    shutdown,
                    attempt_cancelled,
                )?,
            });
        }
        scores.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.occurrence_id.cmp(&right.occurrence_id))
        });
        Ok(WireRerankResponse {
            version: request.version(),
            request_id: request.request_id().get(),
            model: self.model.clone(),
            model_revision: self.model_revision,
            scores,
        })
    }

    fn score_candidate(
        &self,
        query: &QueryFeatures,
        document: &str,
        shutdown: &AtomicBool,
        attempt_cancelled: &AtomicBool,
    ) -> BackendResult<f64> {
        let mut document_tokens = 0_usize;
        let mut matching_tokens = BTreeSet::new();
        for token in ascii_tokens(document) {
            if cancelled(shutdown, attempt_cancelled) {
                return Err(RerankBackendError::Cancelled);
            }
            document_tokens = document_tokens.checked_add(1).ok_or_else(invalid_work)?;
            if document_tokens > self.max_document_tokens {
                return Err(invalid_work());
            }
            let hash = hash_ascii_fold(token);
            if query.unique_tokens.contains(&hash) {
                matching_tokens.insert(hash);
            }
        }
        let overlap = ratio(matching_tokens.len(), query.unique_tokens.len());
        let exact_phrase = if document.contains(&query.original) {
            1.0
        } else {
            0.0
        };
        let brevity = reciprocal_one_plus(document_tokens)?;
        let linear = self.weights[0]
            + self.weights[1] * overlap
            + self.weights[2] * exact_phrase
            + self.weights[3] * brevity;
        let score = 1.0 / (1.0 + (-linear).exp());
        if !score.is_finite() {
            return Err(invalid_work());
        }
        Ok(score)
    }
}

impl RerankBackend for LinearPairV1 {
    fn model_revision(&self) -> u64 {
        self.model_revision
    }

    fn score(
        &self,
        request: &RerankRequest,
        _deadline_micros: u64,
    ) -> BackendResult<WireRerankResponse> {
        let cancelled = AtomicBool::new(false);
        self.score_with_cancel(request, &cancelled, &cancelled)
    }
}

struct QueryFeatures {
    original: String,
    unique_tokens: BTreeSet<u64>,
}

impl QueryFeatures {
    fn new(
        query: &str,
        maximum_tokens: usize,
        shutdown: &AtomicBool,
        attempt_cancelled: &AtomicBool,
    ) -> BackendResult<Self> {
        let mut token_count = 0_usize;
        let mut unique_tokens = BTreeSet::new();
        for token in ascii_tokens(query) {
            if cancelled(shutdown, attempt_cancelled) {
                return Err(RerankBackendError::Cancelled);
            }
            token_count = token_count.checked_add(1).ok_or_else(invalid_work)?;
            if token_count > maximum_tokens {
                return Err(invalid_work());
            }
            unique_tokens.insert(hash_ascii_fold(token));
        }
        if token_count == 0 {
            return Err(invalid_work());
        }
        Ok(Self {
            original: query.to_owned(),
            unique_tokens,
        })
    }
}

fn cancelled(shutdown: &AtomicBool, attempt_cancelled: &AtomicBool) -> bool {
    shutdown.load(Ordering::Relaxed) || attempt_cancelled.load(Ordering::Relaxed)
}

fn ascii_tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
}

fn hash_ascii_fold(token: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in token.bytes() {
        hash ^= u64::from(byte.to_ascii_lowercase());
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let numerator = numerator as f64;
    #[allow(clippy::cast_precision_loss)]
    let denominator = denominator as f64;
    numerator / denominator
}

fn reciprocal_one_plus(value: usize) -> BackendResult<f64> {
    let denominator = value.checked_add(1).ok_or_else(invalid_work)?;
    #[allow(clippy::cast_precision_loss)]
    let denominator = denominator as f64;
    Ok(1.0 / denominator)
}

fn invalid_work() -> RerankBackendError {
    RerankBackendError::InvalidPlan {
        reason: "request exceeds the selected worker contract".to_owned(),
    }
}
