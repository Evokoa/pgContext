//! Serialized form of the external-rerank envelope.
//!
//! Wire types are plain data with public fields: they are what a provider sees
//! and what a provider sends. They carry no invariants, because a provider is
//! under no obligation to respect any. Invariants are re-established by
//! converting into the validated `context-query` contract, which is the only
//! way a response reaches the executor.

use context_core::{OccurrenceId, PointId, SourceVersion};
use context_query::{
    RerankCandidate, RerankMetadata, RerankRequest, RerankRequestId, RerankResponse, RerankScore,
};
use serde::{Deserialize, Serialize};

/// Maximum bytes retained from a provider-supplied diagnostic string.
///
/// A refusal reason quotes what the provider sent, and a provider can send a
/// megabyte. The reason is bounded here so one hostile response cannot flood a
/// PostgreSQL log through an error message.
pub const MAX_RERANK_DIAGNOSTIC_BYTES: usize = 256;

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

/// Serialized allow-listed metadata pair.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WireRerankMetadata {
    /// Metadata key.
    pub key: String,
    /// Metadata value.
    pub value: String,
}

/// Serialized authorized candidate.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WireRerankCandidate {
    /// Stable occurrence identity.
    pub occurrence_id: u64,
    /// Logical point identity.
    pub point_id: u64,
    /// Source version observed when the envelope was built.
    pub source_version: u64,
    /// Authorized text released to the provider.
    pub text: String,
    /// Allow-listed metadata released to the provider.
    pub metadata: Vec<WireRerankMetadata>,
}

/// Serialized rerank request.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WireRerankRequest {
    /// Envelope contract version.
    pub version: u16,
    /// Request identity a response must echo.
    pub request_id: u64,
    /// Model revision a response must echo.
    pub model_revision: u64,
    /// Wall-clock expiry in the caller's microsecond epoch.
    pub expires_at_micros: u64,
    /// Authorized candidates.
    pub candidates: Vec<WireRerankCandidate>,
}

impl WireRerankRequest {
    /// Serializes a validated request for transmission.
    #[must_use]
    pub fn from_request(request: &RerankRequest) -> Self {
        Self {
            version: request.version(),
            request_id: request.request_id().get(),
            model_revision: request.model_revision(),
            expires_at_micros: request.expires_at_micros(),
            candidates: request
                .candidates()
                .iter()
                .map(|candidate| WireRerankCandidate {
                    occurrence_id: candidate.occurrence_id().get(),
                    point_id: candidate.point_id().get(),
                    source_version: candidate.source_version().get(),
                    text: candidate.text().to_owned(),
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
        let candidates = self
            .candidates
            .iter()
            .map(|candidate| {
                let metadata = candidate
                    .metadata
                    .iter()
                    .map(|entry| {
                        RerankMetadata::new(entry.key.clone(), entry.value.clone()).map_err(invalid)
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
                    candidate.text.clone(),
                    metadata,
                )
                .map_err(invalid)
            })
            .collect::<WireResult<Vec<_>>>()?;
        RerankRequest::new(
            request_identity(self.request_id)?,
            self.model_revision,
            self.expires_at_micros,
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
        serde_json::to_string(self).map_err(malformed)
    }
}

/// Serialized provider score.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct WireRerankScore {
    /// Occurrence this score applies to.
    pub occurrence_id: u64,
    /// Provider score.
    pub score: f64,
}

/// Serialized provider response.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WireRerankResponse {
    /// Envelope contract version the provider answered with.
    pub version: u16,
    /// Request identity the provider echoed.
    pub request_id: u64,
    /// Model revision the provider echoed.
    pub model_revision: u64,
    /// Provider scores, in provider order.
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
        serde_json::from_str(payload).map_err(malformed)
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

fn malformed(error: serde_json::Error) -> WireError {
    // serde embeds the offending value in a type error, so this string is
    // partly provider-authored and gets the same bound as any other.
    WireError::Malformed {
        reason: bound_diagnostic(error.to_string()),
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
            "authorized text",
            vec![RerankMetadata::new("tenant", "acme").expect("metadata")],
        )
        .expect("candidate");
        RerankRequest::new(
            RerankRequestId::new(5).expect("request id"),
            9,
            1_000,
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
        let payload = r#"{"version":1,"request_id":0,"model_revision":9,"scores":[]}"#;
        assert!(matches!(
            WireRerankResponse::from_json(payload)
                .expect("well-formed json")
                .to_response(),
            Err(WireError::ReservedIdentity {
                field: "request_id"
            })
        ));

        let payload = r#"{"version":1,"request_id":5,"model_revision":9,"scores":[{"occurrence_id":0,"score":1.0}]}"#;
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
        // serde embeds the offending value in a type error, so a hostile
        // provider can author most of this string.
        let flood = "x".repeat(50_000);
        let payload =
            format!(r#"{{"version":"{flood}","request_id":5,"model_revision":9,"scores":[]}}"#);
        let WireError::Malformed { reason } =
            WireRerankResponse::from_json(&payload).expect_err("type error")
        else {
            unreachable!("a type mismatch is malformed")
        };
        assert_eq!(reason.len(), MAX_RERANK_DIAGNOSTIC_BYTES);
    }

    #[test]
    fn a_provider_payload_reaches_validation_only_through_the_contract() {
        let request = request();
        let payload = r#"{"version":1,"request_id":5,"model_revision":9,
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
        let mismatched = r#"{"version":1,"request_id":6,"model_revision":9,
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
                        text.clone(),
                        Vec::new(),
                    )
                    .expect("candidate")
                })
                .collect();
            let original = RerankRequest::new(
                RerankRequestId::new(1).expect("request id"),
                1,
                1_000,
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
