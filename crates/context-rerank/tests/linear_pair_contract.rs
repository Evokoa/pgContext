//! Contract tests for the private, operator-artifact-backed P12 adapter.

#![allow(clippy::expect_used)]

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use context_core::{OccurrenceId, PointId, SourceVersion};
use context_query::{
    RERANK_CONTENT_DIGEST_BYTES, RerankCandidate, RerankContentDigest, RerankContribution,
    RerankModelName, RerankQuery, RerankRequest, RerankRequestId,
};
use pgcontext_worker::{
    LinearPairV1, LinearPairV1Error, RerankBackend, RerankBackendError, VerifiedArtifact,
    WireRerankRequest, WorkerManifest,
};
use sha2::{Digest, Sha256};

static TEMP_ID: AtomicU64 = AtomicU64::new(1);

struct Fixture {
    root: PathBuf,
    manifest: WorkerManifest,
}

impl Fixture {
    fn new(weights: [f64; 4], query_tokens: usize, document_tokens: usize) -> Self {
        Self::with_tokenizer(weights, query_tokens, document_tokens, "ascii_tokens_v1")
    }

    fn with_tokenizer(
        weights: [f64; 4],
        query_tokens: usize,
        document_tokens: usize,
        tokenizer_revision: &str,
    ) -> Self {
        let root = std::env::temp_dir().join(format!(
            "pgcontext-linear-pair-{}-{}",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("fixture directory");
        let artifact = artifact(weights);
        fs::write(root.join("weights.bin"), &artifact).expect("fixture artifact");
        let digest = Sha256::digest(&artifact)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let manifest = WorkerManifest::from_json(&format!(
            r#"{{
              "schema_version":1,
              "adapter":"linear_pair_v1",
              "model":"fixture-reranker",
              "model_revision":7,
              "artifact_path":"weights.bin",
              "artifact_bytes":{},
              "artifact_sha256":"{digest}",
              "tokenizer_revision":"{tokenizer_revision}",
              "input_contract":"rerank_envelope_v3",
              "output_contract":"rerank_response_v3",
              "failure_contract":"rerank_failure_v1",
              "score_contract":"higher_is_better_unit_interval",
              "max_request_bytes":4194304,
              "max_candidates":64,
              "max_query_tokens":{query_tokens},
              "max_document_tokens":{document_tokens},
              "max_elapsed_micros":1000000,
              "max_retries":1,
              "breaker_failure_threshold":3,
              "breaker_cooldown_micros":5000000,
              "supported_platforms":["{}"],
              "license_spdx":"Apache-2.0",
              "license_url":"https://www.apache.org/licenses/LICENSE-2.0",
              "distribution":"operator_provided_only"
            }}"#,
            artifact.len(),
            pgcontext_worker::current_platform_id()
        ))
        .expect("manifest");
        Self { root, manifest }
    }

    fn load(&self) -> Result<LinearPairV1, LinearPairV1Error> {
        let artifact = VerifiedArtifact::load(&self.manifest, &self.root).expect("artifact");
        LinearPairV1::load(&self.manifest, artifact)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn artifact(weights: [f64; 4]) -> Vec<u8> {
    let mut bytes = b"PGLPAIR1".to_vec();
    for weight in weights {
        bytes.extend_from_slice(&weight.to_le_bytes());
    }
    bytes
}

fn candidate(id: u64, text: &str) -> RerankCandidate {
    RerankCandidate::new(
        OccurrenceId::new(id).expect("occurrence"),
        PointId::new(id),
        SourceVersion::new(1).expect("source version"),
        RerankContentDigest::new([1; RERANK_CONTENT_DIGEST_BYTES]),
        text,
        usize::try_from(id).expect("rank"),
        0.1,
        vec![RerankContribution::new("fixture", 1, 0.1, 1.0, 0.01).expect("contribution")],
        Vec::new(),
    )
    .expect("candidate")
}

fn request(query: &str, candidates: Vec<RerankCandidate>) -> RerankRequest {
    RerankRequest::new(
        RerankRequestId::new(1).expect("request"),
        RerankModelName::new("fixture-reranker").expect("model"),
        7,
        1_000_000,
        RerankQuery::new(query).expect("query"),
        candidates,
    )
    .expect("request")
}

#[test]
fn explicit_verified_weights_produce_joint_query_document_scores() {
    let fixture = Fixture::new([-2.0, 6.0, 1.0, 0.0], 8, 32);
    let backend = fixture.load().expect("adapter");
    let request = request(
        "postgres retrieval",
        vec![
            candidate(1, "postgres retrieval inside the database"),
            candidate(2, "unrelated cooking notes"),
        ],
    );

    let response = backend.score(&request, 1_000_000).expect("score");
    assert_eq!(response.scores.len(), 2);
    assert!(response.scores[0].score > response.scores[1].score);
    assert!(response.scores.iter().all(|score| score.score.is_finite()));
    assert!(
        response
            .scores
            .iter()
            .all(|score| (0.0..=1.0).contains(&score.score))
    );

    let repeated = backend.score(&request, 1_000_000).expect("repeat");
    assert_eq!(response, repeated, "fixture scoring must be deterministic");
    let round_trip = WireRerankRequest::from_request(&request)
        .to_request()
        .expect("wire request");
    assert_eq!(round_trip, request);
}

#[test]
fn malformed_or_nonfinite_artifacts_never_construct_an_adapter() {
    let short = Fixture::new([1.0, 2.0, 3.0, 4.0], 8, 32);
    fs::write(short.root.join("weights.bin"), b"PGLPAIR1").expect("replace artifact");
    assert!(VerifiedArtifact::load(&short.manifest, &short.root).is_err());

    let nonfinite = Fixture::new([f64::NAN, 2.0, 3.0, 4.0], 8, 32);
    assert!(matches!(
        nonfinite.load(),
        Err(LinearPairV1Error::InvalidArtifact)
    ));

    let wrong_tokenizer =
        Fixture::with_tokenizer([1.0, 2.0, 3.0, 4.0], 8, 32, "decorative-tokenizer");
    assert!(matches!(
        wrong_tokenizer.load(),
        Err(LinearPairV1Error::InvalidArtifact)
    ));
}

#[test]
fn token_and_model_bounds_fail_before_partial_provider_output() {
    let fixture = Fixture::new([-2.0, 6.0, 1.0, 0.0], 2, 2);
    let backend = fixture.load().expect("adapter");
    let too_many_query_tokens = request("one two three", vec![candidate(1, "one two")]);
    assert!(matches!(
        backend.score(&too_many_query_tokens, 1_000_000),
        Err(RerankBackendError::InvalidPlan { .. })
    ));

    let too_many_document_tokens = request("one two", vec![candidate(1, "one two three")]);
    assert!(matches!(
        backend.score(&too_many_document_tokens, 1_000_000),
        Err(RerankBackendError::InvalidPlan { .. })
    ));

    let mut wrong_model =
        WireRerankRequest::from_request(&request("one", vec![candidate(1, "one")]))
            .to_request()
            .expect("wire request");
    wrong_model = RerankRequest::new(
        wrong_model.request_id(),
        RerankModelName::new("different-model").expect("model"),
        wrong_model.model_revision(),
        wrong_model.expires_at_micros(),
        wrong_model.query().clone(),
        wrong_model.candidates().to_vec(),
    )
    .expect("different model request");
    assert!(matches!(
        backend.score(&wrong_model, 1_000_000),
        Err(RerankBackendError::InvalidPlan { .. })
    ));
}
