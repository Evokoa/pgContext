//! Serialized form of the external-rerank envelope.
//!
//! Wire types are plain data with public fields: they are what a provider sees
//! and what a provider sends. They carry no invariants, because a provider is
//! under no obligation to respect any. Invariants are re-established by
//! converting into the validated `context-query` contract, which is the only
//! way a response reaches the executor.

use context_core::{OccurrenceId, PointId, SourceVersion};
pub use context_query::MAX_RERANK_WIRE_BYTES;
use context_query::{
    MAX_RERANK_CANDIDATES, MAX_RERANK_CONTRIBUTIONS, MAX_RERANK_METADATA_ENTRIES,
    RERANK_CONTENT_DIGEST_BYTES, RERANK_ENVELOPE_VERSION, RerankCandidate, RerankContentDigest,
    RerankContribution, RerankMetadata, RerankModelName, RerankQuery, RerankRequest,
    RerankRequestId, RerankResponse, RerankScore,
};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{Error as _, SeqAccess, Visitor},
};

/// Maximum bytes retained from a provider-supplied diagnostic string.
///
/// A refusal reason quotes what the provider sent, and a provider can send a
/// megabyte. The reason is bounded here so one hostile response cannot flood a
/// PostgreSQL log through an error message.
pub const MAX_RERANK_DIAGNOSTIC_BYTES: usize = 256;
/// Version of the persistent worker's operational-failure frame.
pub const RERANK_FAILURE_FRAME_VERSION: u16 = 1;

/// Truncates a diagnostic to [`MAX_RERANK_DIAGNOSTIC_BYTES`], never splitting a
/// character.
#[must_use]
pub fn bound_diagnostic(reason: String) -> String {
    if reason.len() <= MAX_RERANK_DIAGNOSTIC_BYTES {
        return reason;
    }
    let mut end = MAX_RERANK_DIAGNOSTIC_BYTES;
    while end > 0 && !reason.is_char_boundary(end) {
        end -= 1;
    }
    let mut bounded = reason;
    bounded.truncate(end);
    bounded
}

/// Why a wire value could not be converted into the validated contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WireError {
    /// A nonzero identity arrived as zero.
    ReservedIdentity {
        /// Field that carried the reserved value.
        field: &'static str,
    },
    /// A validated constructor rejected the wire value.
    Invalid {
        /// Stable reason from the validating constructor.
        reason: String,
    },
    /// The payload was not well-formed JSON, or did not match the schema.
    Malformed {
        /// Serializer-provided detail.
        reason: String,
    },
}

impl core::fmt::Display for WireError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ReservedIdentity { field } => {
                write!(formatter, "rerank wire {field} used a reserved identity")
            }
            Self::Invalid { reason } | Self::Malformed { reason } => {
                write!(formatter, "rerank wire payload rejected: {reason}")
            }
        }
    }
}

impl std::error::Error for WireError {}

/// Result type for wire conversion.
pub type WireResult<T> = Result<T, WireError>;

/// Versioned, request-correlated operational failure from the persistent worker.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WireRerankFailure {
    /// Failure-frame schema version.
    pub version: u16,
    /// Validated request identity this failure answers.
    pub request_id: u64,
    /// Stable worker error class.
    pub error: String,
    /// Exact SQL-compatible semantic-rerank finalization reason.
    pub failure_reason: String,
}

impl WireRerankFailure {
    /// Builds a correlated frame only for an operationally degradable failure.
    #[must_use]
    pub fn from_error(request_id: RerankRequestId, error: crate::WorkerRunError) -> Option<Self> {
        let failure_reason = error.finalization_failure_reason()?;
        Some(Self {
            version: RERANK_FAILURE_FRAME_VERSION,
            request_id: request_id.get(),
            error: error.stable_name().to_owned(),
            failure_reason: failure_reason.to_owned(),
        })
    }

    /// Parses and validates one bounded failure frame.
    ///
    /// # Errors
    ///
    /// Returns [`WireError::Malformed`] for an unknown version, reserved
    /// request identity, unknown class, mismatched reason, or malformed JSON.
    pub fn from_json(payload: &str) -> WireResult<Self> {
        if payload.len() > MAX_RERANK_DIAGNOSTIC_BYTES {
            return Err(malformed_payload());
        }
        let frame: Self = serde_json::from_str(payload).map_err(malformed)?;
        let valid = frame.version == RERANK_FAILURE_FRAME_VERSION
            && frame.request_id > 0
            && matches!(
                (frame.error.as_str(), frame.failure_reason.as_str()),
                ("expired", "expired")
                    | ("timeout", "timeout")
                    | ("unavailable", "unavailable")
                    | ("circuit_open", "unavailable")
                    | ("crash", "crash")
                    | ("partial_output", "partial_output")
            );
        if !valid {
            return Err(malformed_payload());
        }
        Ok(frame)
    }

    /// Serializes a bounded validated failure frame.
    ///
    /// # Errors
    ///
    /// Returns [`WireError::Malformed`] if serialization exceeds the fixed
    /// diagnostic-frame ceiling.
    pub fn to_json(&self) -> WireResult<String> {
        let payload = serde_json::to_string(self).map_err(malformed)?;
        if payload.len() > MAX_RERANK_DIAGNOSTIC_BYTES {
            return Err(malformed_payload());
        }
        Ok(payload)
    }
}

/// Serialized allow-listed metadata pair.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WireRerankMetadata {
    /// Metadata key.
    pub key: String,
    /// Metadata value.
    pub value: String,
}

/// Serialized authorized candidate.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WireRerankCandidate {
    /// Stable occurrence identity.
    pub occurrence_id: u64,
    /// Logical point identity.
    pub point_id: u64,
    /// Source version observed when the envelope was built.
    pub source_version: u64,
    /// Fixed authoritative content digest.
    pub content_digest: [u8; RERANK_CONTENT_DIGEST_BYTES],
    /// Authorized text released to the provider.
    pub text: String,
    /// One-based fused rank before semantic reranking.
    pub fused_rank: usize,
    /// Fused score retained for fallback and diagnostics.
    pub fused_score: f64,
    /// Per-profile rank-fusion evidence.
    #[serde(deserialize_with = "deserialize_contributions")]
    pub contributions: Vec<WireRerankContribution>,
    /// Allow-listed metadata released to the provider.
    #[serde(deserialize_with = "deserialize_metadata")]
    pub metadata: Vec<WireRerankMetadata>,
}

/// Serialized rank-fusion contribution.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WireRerankContribution {
    /// Immutable embedding profile name.
    pub profile: String,
    /// One-based branch rank.
    pub rank: usize,
    /// Diagnostic native branch score.
    pub native_score: f64,
    /// Declared RRF weight.
    pub weight: f64,
    /// Weighted RRF contribution.
    pub contribution: f64,
}

/// Serialized rerank request.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WireRerankRequest {
    /// Envelope contract version.
    pub version: u16,
    /// Request identity a response must echo.
    pub request_id: u64,
    /// Immutable model identity.
    pub model: String,
    /// Model revision a response must echo.
    pub model_revision: u64,
    /// Wall-clock expiry in the caller's microsecond epoch.
    pub expires_at_micros: u64,
    /// Original query released to the provider.
    pub query: String,
    /// Authorized candidates.
    #[serde(deserialize_with = "deserialize_candidates")]
    pub candidates: Vec<WireRerankCandidate>,
}

impl WireRerankRequest {
    /// Parses a bounded untrusted worker request.
    ///
    /// # Errors
    ///
    /// Returns [`WireError::Malformed`] before parsing when the payload exceeds
    /// [`MAX_RERANK_WIRE_BYTES`], or when JSON does not match the exact schema.
    pub fn from_json(payload: &str) -> WireResult<Self> {
        if payload.len() > MAX_RERANK_WIRE_BYTES {
            return Err(malformed_payload());
        }
        serde_json::from_str(payload).map_err(malformed)
    }

    /// Serializes a validated request for transmission.
    #[must_use]
    pub fn from_request(request: &RerankRequest) -> Self {
        Self {
            version: request.version(),
            request_id: request.request_id().get(),
            model: request.model().as_str().to_owned(),
            model_revision: request.model_revision(),
            expires_at_micros: request.expires_at_micros(),
            query: request.query().as_str().to_owned(),
            candidates: request
                .candidates()
                .iter()
                .map(|candidate| WireRerankCandidate {
                    occurrence_id: candidate.occurrence_id().get(),
                    point_id: candidate.point_id().get(),
                    source_version: candidate.source_version().get(),
                    content_digest: *candidate.content_digest().as_bytes(),
                    text: candidate.text().to_owned(),
                    fused_rank: candidate.fused_rank(),
                    fused_score: candidate.fused_score(),
                    contributions: candidate
                        .contributions()
                        .iter()
                        .map(|contribution| WireRerankContribution {
                            profile: contribution.profile().to_owned(),
                            rank: contribution.rank(),
                            native_score: contribution.native_score(),
                            weight: contribution.weight(),
                            contribution: contribution.contribution(),
                        })
                        .collect(),
                    metadata: candidate
                        .metadata()
                        .iter()
                        .map(|entry| WireRerankMetadata {
                            key: entry.key().to_owned(),
                            value: entry.value().to_owned(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    /// Rebuilds the validated request contract from wire data.
    ///
    /// This exists for a provider implementation and for round-trip tests; the
    /// executor never accepts a request from the wire.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when an identity is reserved or a validated
    /// constructor rejects the value.
    pub fn to_request(&self) -> WireResult<RerankRequest> {
        self.clone().into_request()
    }

    /// Consumes wire data while rebuilding the validated request contract.
    ///
    /// This is the production conversion path: it moves authorized strings
    /// instead of retaining a second complete wire copy beside the request.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when an identity is reserved or a validated
    /// constructor rejects the value.
    pub fn into_request(self) -> WireResult<RerankRequest> {
        if self.version != RERANK_ENVELOPE_VERSION {
            return Err(WireError::Invalid {
                reason: "unsupported rerank envelope version".to_owned(),
            });
        }
        let candidates = self
            .candidates
            .into_iter()
            .map(|candidate| {
                let metadata = candidate
                    .metadata
                    .into_iter()
                    .map(|entry| RerankMetadata::new(entry.key, entry.value).map_err(invalid))
                    .collect::<WireResult<Vec<_>>>()?;
                let contributions = candidate
                    .contributions
                    .into_iter()
                    .map(|contribution| {
                        RerankContribution::new(
                            contribution.profile,
                            contribution.rank,
                            contribution.native_score,
                            contribution.weight,
                            contribution.contribution,
                        )
                        .map_err(invalid)
                    })
                    .collect::<WireResult<Vec<_>>>()?;
                RerankCandidate::new(
                    identity(candidate.occurrence_id, "occurrence_id", OccurrenceId::new)?,
                    PointId::new(candidate.point_id),
                    identity(
                        candidate.source_version,
                        "source_version",
                        SourceVersion::new,
                    )?,
                    RerankContentDigest::new(candidate.content_digest),
                    candidate.text,
                    candidate.fused_rank,
                    candidate.fused_score,
                    contributions,
                    metadata,
                )
                .map_err(invalid)
            })
            .collect::<WireResult<Vec<_>>>()?;
        RerankRequest::new(
            request_identity(self.request_id)?,
            RerankModelName::new(self.model).map_err(invalid)?,
            self.model_revision,
            self.expires_at_micros,
            RerankQuery::new(self.query).map_err(invalid)?,
            candidates,
        )
        .map_err(invalid)
    }

    /// Serializes this request to JSON.
    ///
    /// # Errors
    ///
    /// Returns [`WireError::Malformed`] when serialization fails.
    pub fn to_json(&self) -> WireResult<String> {
        let payload = serde_json::to_string(self).map_err(malformed)?;
        if payload.len() > MAX_RERANK_WIRE_BYTES {
            return Err(malformed_payload());
        }
        Ok(payload)
    }
}

fn deserialize_candidates<'de, D>(deserializer: D) -> Result<Vec<WireRerankCandidate>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_vec(deserializer, MAX_RERANK_CANDIDATES)
}

fn deserialize_contributions<'de, D>(
    deserializer: D,
) -> Result<Vec<WireRerankContribution>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_vec(deserializer, MAX_RERANK_CONTRIBUTIONS)
}

fn deserialize_metadata<'de, D>(deserializer: D) -> Result<Vec<WireRerankMetadata>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_vec(deserializer, MAX_RERANK_METADATA_ENTRIES)
}

fn deserialize_scores<'de, D>(deserializer: D) -> Result<Vec<WireRerankScore>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_vec(deserializer, MAX_RERANK_CANDIDATES)
}

fn deserialize_bounded_vec<'de, D, T>(deserializer: D, maximum: usize) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct BoundedVecVisitor<T> {
        maximum: usize,
        marker: core::marker::PhantomData<T>,
    }

    impl<'de, T> Visitor<'de> for BoundedVecVisitor<T>
    where
        T: Deserialize<'de>,
    {
        type Value = Vec<T>;

        fn expecting(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(formatter, "an array with at most {} entries", self.maximum)
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut values =
                Vec::with_capacity(sequence.size_hint().unwrap_or(0).min(self.maximum));
            while values.len() < self.maximum {
                let Some(value) = sequence.next_element()? else {
                    return Ok(values);
                };
                values.push(value);
            }
            if sequence.next_element::<T>()?.is_some() {
                return Err(A::Error::custom("bounded rerank array exceeded"));
            }
            Ok(values)
        }
    }

    deserializer.deserialize_seq(BoundedVecVisitor {
        maximum,
        marker: core::marker::PhantomData,
    })
}

/// Serialized provider score.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WireRerankScore {
    /// Occurrence this score applies to.
    pub occurrence_id: u64,
    /// Provider score.
    pub score: f64,
}

/// Serialized provider response.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WireRerankResponse {
    /// Envelope contract version the provider answered with.
    pub version: u16,
    /// Request identity the provider echoed.
    pub request_id: u64,
    /// Immutable model identity the provider echoed.
    pub model: String,
    /// Model revision the provider echoed.
    pub model_revision: u64,
    /// Provider scores, in provider order.
    #[serde(deserialize_with = "deserialize_scores")]
    pub scores: Vec<WireRerankScore>,
}

impl WireRerankResponse {
    /// Parses an untrusted provider payload.
    ///
    /// # Errors
    ///
    /// Returns [`WireError::Malformed`] for a payload that is not well-formed
    /// JSON matching the schema.
    pub fn from_json(payload: &str) -> WireResult<Self> {
        if payload.len() > MAX_RERANK_WIRE_BYTES {
            return Err(malformed_payload());
        }
        serde_json::from_str(payload).map_err(malformed)
    }

    /// Serializes a worker response.
    ///
    /// # Errors
    ///
    /// Returns [`WireError::Malformed`] when serialization fails or exceeds
    /// the bounded wire payload.
    pub fn to_json(&self) -> WireResult<String> {
        let payload = serde_json::to_string(self).map_err(malformed)?;
        if payload.len() > MAX_RERANK_WIRE_BYTES {
            return Err(malformed_payload());
        }
        Ok(payload)
    }

    /// Converts an untrusted payload into the validated response contract.
    ///
    /// A reserved occurrence identity or a non-finite score is refused here, so
    /// those values never reach the executor's membership and expiry checks.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] for a reserved identity or a non-finite score.
    pub fn to_response(&self) -> WireResult<RerankResponse> {
        let scores = self
            .scores
            .iter()
            .map(|score| {
                RerankScore::new(
                    identity(score.occurrence_id, "occurrence_id", OccurrenceId::new)?,
                    score.score,
                )
                .map_err(invalid)
            })
            .collect::<WireResult<Vec<_>>>()?;
        Ok(RerankResponse::new(
            self.version,
            request_identity(self.request_id)?,
            RerankModelName::new(self.model.clone()).map_err(invalid)?,
            self.model_revision,
            scores,
        ))
    }
}

fn identity<T>(value: u64, field: &'static str, build: impl Fn(u64) -> Option<T>) -> WireResult<T> {
    build(value).ok_or(WireError::ReservedIdentity { field })
}

fn request_identity(value: u64) -> WireResult<RerankRequestId> {
    identity(value, "request_id", |candidate| {
        RerankRequestId::new(candidate).ok()
    })
}

fn invalid(error: context_query::QueryError) -> WireError {
    WireError::Invalid {
        reason: bound_diagnostic(error.to_string()),
    }
}

fn malformed(_error: serde_json::Error) -> WireError {
    malformed_payload()
}

fn malformed_payload() -> WireError {
    WireError::Malformed {
        reason: "malformed payload".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn request() -> RerankRequest {
        let candidate = RerankCandidate::new(
            OccurrenceId::new(11).expect("occurrence"),
            PointId::new(11),
            SourceVersion::new(3).expect("version"),
            RerankContentDigest::new([3; RERANK_CONTENT_DIGEST_BYTES]),
            "authorized text",
            1,
            0.25,
            vec![RerankContribution::new("legacy_v1", 1, 0.4, 1.0, 0.01).expect("contribution")],
            vec![RerankMetadata::new("tenant", "acme").expect("metadata")],
        )
        .expect("candidate");
        RerankRequest::new(
            RerankRequestId::new(5).expect("request id"),
            RerankModelName::new("fixture-v1").expect("model"),
            9,
            1_000,
            RerankQuery::new("postgres retrieval").expect("query"),
            vec![candidate],
        )
        .expect("request")
    }

    #[test]
    fn a_request_round_trips_through_the_wire_unchanged() {
        let original = request();
        let rebuilt = WireRerankRequest::from_request(&original)
            .to_request()
            .expect("round trip");
        assert_eq!(rebuilt, original);
    }

    #[test]
    fn a_request_round_trips_through_json() {
        let wire = WireRerankRequest::from_request(&request());
        let json = wire.to_json().expect("json");
        let parsed = serde_json::from_str::<WireRerankRequest>(&json).expect("parse");
        assert_eq!(parsed, wire);
    }

    #[test]
    fn a_malformed_payload_is_refused_rather_than_guessed_at() {
        assert!(matches!(
            WireRerankResponse::from_json("not json"),
            Err(WireError::Malformed { .. })
        ));
        assert!(matches!(
            WireRerankResponse::from_json(r#"{"version":1}"#),
            Err(WireError::Malformed { .. })
        ));
    }

    #[test]
    fn reserved_identities_are_refused_at_the_boundary() {
        let payload =
            r#"{"version":3,"request_id":0,"model":"fixture-v1","model_revision":9,"scores":[]}"#;
        assert!(matches!(
            WireRerankResponse::from_json(payload)
                .expect("well-formed json")
                .to_response(),
            Err(WireError::ReservedIdentity {
                field: "request_id"
            })
        ));

        let payload = r#"{"version":3,"request_id":5,"model":"fixture-v1","model_revision":9,"scores":[{"occurrence_id":0,"score":1.0}]}"#;
        assert!(matches!(
            WireRerankResponse::from_json(payload)
                .expect("well-formed json")
                .to_response(),
            Err(WireError::ReservedIdentity {
                field: "occurrence_id"
            })
        ));
    }

    #[test]
    fn a_non_finite_provider_score_never_reaches_the_contract() {
        // JSON has no NaN literal, so this arrives via a transport that decodes
        // an out-of-range magnitude to infinity rather than refusing it.
        for score in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let response = WireRerankResponse {
                version: 1,
                request_id: 5,
                model: "fixture-v1".to_owned(),
                model_revision: 9,
                scores: vec![WireRerankScore {
                    occurrence_id: 11,
                    score,
                }],
            };
            assert!(
                matches!(response.to_response(), Err(WireError::Invalid { .. })),
                "{score} must not reach the validated contract"
            );
        }
    }

    #[test]
    fn wire_data_carrying_a_contract_violation_is_refused_on_conversion() {
        let mut wire = WireRerankRequest::from_request(&request());
        wire.candidates.push(wire.candidates[0].clone());
        assert!(
            matches!(wire.to_request(), Err(WireError::Invalid { .. })),
            "a repeated occurrence must not survive the wire"
        );
    }

    #[test]
    fn a_refusal_cannot_quote_an_unbounded_provider_payload_into_a_log() {
        // serde embeds the offending value in a type error. The wire boundary
        // deliberately discards that diagnostic so provider content never
        // reaches a log, even in bounded form.
        let flood = "x".repeat(50_000);
        let payload =
            format!(r#"{{"version":"{flood}","request_id":5,"model_revision":9,"scores":[]}}"#);
        let WireError::Malformed { reason } =
            WireRerankResponse::from_json(&payload).expect_err("type error")
        else {
            unreachable!("a type mismatch is malformed")
        };
        assert_eq!(reason, "malformed payload");
        assert!(!reason.contains(&flood));
    }

    #[test]
    fn wire_arrays_and_high_escape_payloads_fail_before_unbounded_growth() {
        let scores = (1..=MAX_RERANK_CANDIDATES + 1)
            .map(|occurrence_id| format!(r#"{{"occurrence_id":{occurrence_id},"score":0.1}}"#))
            .collect::<Vec<_>>()
            .join(",");
        let payload = format!(
            r#"{{"version":3,"request_id":5,"model":"fixture-v1","model_revision":9,"scores":[{scores}]}}"#
        );
        assert!(matches!(
            WireRerankResponse::from_json(&payload),
            Err(WireError::Malformed { .. })
        ));

        let candidates = (1_u64..=110)
            .map(|id| {
                RerankCandidate::new(
                    OccurrenceId::new(id).expect("occurrence"),
                    PointId::new(id),
                    SourceVersion::new(1).expect("version"),
                    RerankContentDigest::new([1; RERANK_CONTENT_DIGEST_BYTES]),
                    "\u{0001}".repeat(context_query::MAX_RERANK_TEXT_BYTES),
                    usize::try_from(id).expect("rank"),
                    0.1,
                    vec![
                        RerankContribution::new("fixture", 1, 0.1, 1.0, 0.01)
                            .expect("contribution"),
                    ],
                    Vec::new(),
                )
                .expect("individually bounded candidate")
            })
            .collect::<Vec<_>>();
        let request = RerankRequest::new(
            RerankRequestId::new(5).expect("request"),
            RerankModelName::new("fixture-v1").expect("model"),
            9,
            1_000,
            RerankQuery::new("query").expect("query"),
            candidates,
        )
        .expect("decoded request stays below four MiB");
        assert!(
            WireRerankRequest::from_request(&request).to_json().is_err(),
            "JSON escaping must not bypass the encoded wire ceiling"
        );
    }

    #[test]
    fn operational_failure_frames_are_bounded_correlated_and_round_trip() {
        let frame = WireRerankFailure::from_error(
            RerankRequestId::new(9).expect("request"),
            crate::WorkerRunError::CircuitOpen,
        )
        .expect("operational failure");
        let json = frame.to_json().expect("failure JSON");
        assert_eq!(
            WireRerankFailure::from_json(&json).expect("round trip"),
            frame
        );
        assert_eq!(frame.request_id, 9);
        assert_eq!(frame.failure_reason, "unavailable");
        assert!(
            WireRerankFailure::from_error(
                RerankRequestId::new(9).expect("request"),
                crate::WorkerRunError::InvalidRequest,
            )
            .is_none()
        );
        assert!(
            WireRerankFailure::from_json(
                r#"{"version":1,"request_id":0,"error":"timeout","failure_reason":"timeout"}"#
            )
            .is_err()
        );
    }

    #[test]
    fn a_provider_payload_reaches_validation_only_through_the_contract() {
        let request = request();
        let payload = r#"{"version":3,"request_id":5,"model":"fixture-v1","model_revision":9,
             "scores":[{"occurrence_id":11,"score":0.25}]}"#;
        let response = WireRerankResponse::from_json(payload)
            .expect("well-formed json")
            .to_response()
            .expect("contract");
        let scores =
            context_query::validate_rerank_response(&request, &response, 500).expect("accepted");
        assert_eq!(scores.len(), 1);
        assert_eq!(
            scores[0].occurrence_id(),
            OccurrenceId::new(11).expect("id")
        );

        // The same bytes against a request they do not answer.
        let mismatched = r#"{"version":3,"request_id":6,"model":"fixture-v1","model_revision":9,
             "scores":[{"occurrence_id":11,"score":0.25}]}"#;
        let response = WireRerankResponse::from_json(mismatched)
            .expect("well-formed json")
            .to_response()
            .expect("contract");
        assert!(context_query::validate_rerank_response(&request, &response, 500).is_err());
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(256))]

        /// Arbitrary bytes must produce a refusal or a valid value, never a
        /// panic and never a value that skipped a constructor.
        #[test]
        fn arbitrary_payloads_are_parsed_or_refused_but_never_panic(payload in ".{0,512}") {
            if let Ok(wire) = WireRerankResponse::from_json(&payload)
                && let Ok(response) = wire.to_response()
            {
                proptest::prop_assert!(
                    response.scores().iter().all(|score| score.score().is_finite())
                );
            }
        }

        /// Any request that survives the wire is byte-identical to the original.
        #[test]
        fn any_valid_request_round_trips_through_json(
            occurrences in proptest::collection::vec(1_u64..1_000, 1..16),
            text in ".{0,64}",
        ) {
            let mut unique = occurrences;
            unique.sort_unstable();
            unique.dedup();
            let candidates = unique
                .iter()
                .map(|occurrence| {
                    RerankCandidate::new(
                        OccurrenceId::new(*occurrence).expect("occurrence"),
                        PointId::new(*occurrence),
                        SourceVersion::new(1).expect("version"),
                        RerankContentDigest::new([1; RERANK_CONTENT_DIGEST_BYTES]),
                        text.clone(),
                        usize::try_from(*occurrence).expect("rank"),
                        0.1,
                        vec![RerankContribution::new(
                            "fixture",
                            usize::try_from(*occurrence).expect("rank"),
                            0.1,
                            1.0,
                            0.01,
                        )
                        .expect("contribution")],
                        Vec::new(),
                    )
                    .expect("candidate")
                })
                .collect();
            let original = RerankRequest::new(
                RerankRequestId::new(1).expect("request id"),
                RerankModelName::new("fixture").expect("model"),
                1,
                1_000,
                RerankQuery::new("query").expect("query"),
                candidates,
            )
            .expect("request");

            let json = WireRerankRequest::from_request(&original).to_json().expect("json");
            let rebuilt = serde_json::from_str::<WireRerankRequest>(&json)
                .expect("parse")
                .to_request()
                .expect("contract");
            proptest::prop_assert_eq!(rebuilt, original);
        }
    }
}
