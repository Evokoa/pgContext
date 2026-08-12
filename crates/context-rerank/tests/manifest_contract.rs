//! Contract tests for immutable worker manifests and artifacts.

#![allow(clippy::expect_used)]

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use pgcontext_worker::{ManifestError, VerifiedArtifact, WorkerManifest};
use sha2::{Digest, Sha256};

static TEMP_ID: AtomicU64 = AtomicU64::new(1);

struct Fixture {
    root: PathBuf,
    manifest: String,
    artifact: Vec<u8>,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "pgcontext-worker-manifest-{}-{}",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("fixture directory");
        let artifact = b"operator-provided-test-weights".to_vec();
        fs::write(root.join("weights.bin"), &artifact).expect("fixture artifact");
        let digest = hex_digest(&artifact);
        let platform = pgcontext_worker::current_platform_id();
        let manifest = format!(
            r#"{{
              "schema_version": 1,
              "adapter": "linear_pair_v1",
              "model": "fixture-reranker",
              "model_revision": 7,
              "artifact_path": "weights.bin",
              "artifact_bytes": {},
              "artifact_sha256": "{digest}",
              "tokenizer_revision": "ascii_tokens_v1",
              "input_contract": "rerank_envelope_v3",
              "output_contract": "rerank_response_v3",
              "failure_contract": "rerank_failure_v1",
              "score_contract": "higher_is_better_unit_interval",
              "max_request_bytes": 4194304,
              "max_candidates": 64,
              "max_query_tokens": 128,
              "max_document_tokens": 512,
              "max_elapsed_micros": 1000000,
              "max_retries": 1,
              "breaker_failure_threshold": 3,
              "breaker_cooldown_micros": 5000000,
              "supported_platforms": ["{platform}"],
              "license_spdx": "Apache-2.0",
              "license_url": "https://www.apache.org/licenses/LICENSE-2.0",
              "distribution": "operator_provided_only"
            }}"#,
            artifact.len()
        );
        Self {
            root,
            manifest,
            artifact,
        }
    }

    fn parse(&self) -> WorkerManifest {
        WorkerManifest::from_json(&self.manifest).expect("valid manifest")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn replace_field(manifest: &str, from: &str, to: &str) -> String {
    let replaced = manifest.replacen(from, to, 1);
    assert_ne!(manifest, replaced, "fixture replacement must apply");
    replaced
}

#[test]
fn a_valid_manifest_loads_only_the_digest_verified_operator_artifact() {
    let fixture = Fixture::new();
    let manifest = fixture.parse();
    let verified = VerifiedArtifact::load(&manifest, &fixture.root).expect("verified artifact");

    assert_eq!(verified.bytes(), fixture.artifact);
    assert_eq!(manifest.model().as_str(), "fixture-reranker");
    assert_eq!(manifest.model_revision(), 7);
    assert_eq!(manifest.artifact_sha256(), hex_digest(&fixture.artifact));
    assert_eq!(manifest.max_retries(), 1);
    assert_eq!(manifest.breaker_failure_threshold(), 3);
    assert_eq!(manifest.breaker_cooldown_micros(), 5_000_000);
}

#[test]
fn artifact_length_digest_and_path_escape_each_fail_closed() {
    let fixture = Fixture::new();

    let wrong_length = replace_field(
        &fixture.manifest,
        &format!("\"artifact_bytes\": {}", fixture.artifact.len()),
        "\"artifact_bytes\": 1",
    );
    let manifest = WorkerManifest::from_json(&wrong_length).expect("shape remains valid");
    assert!(matches!(
        VerifiedArtifact::load(&manifest, &fixture.root),
        Err(ManifestError::ArtifactLengthMismatch)
    ));

    let wrong_digest = replace_field(
        &fixture.manifest,
        &hex_digest(&fixture.artifact),
        &"00".repeat(32),
    );
    let manifest = WorkerManifest::from_json(&wrong_digest).expect("shape remains valid");
    assert!(matches!(
        VerifiedArtifact::load(&manifest, &fixture.root),
        Err(ManifestError::ArtifactDigestMismatch)
    ));

    let escaped = replace_field(&fixture.manifest, "weights.bin", "../weights.bin");
    assert!(matches!(
        WorkerManifest::from_json(&escaped),
        Err(ManifestError::InvalidManifest)
    ));
}

#[test]
fn manifests_reject_unknown_fields_unbounded_limits_and_redistributable_weights() {
    let fixture = Fixture::new();
    let unknown = replace_field(
        &fixture.manifest,
        "\"schema_version\": 1,",
        "\"schema_version\": 1, \"surprise\": true,",
    );
    assert!(matches!(
        WorkerManifest::from_json(&unknown),
        Err(ManifestError::InvalidManifest)
    ));

    let candidates = replace_field(
        &fixture.manifest,
        "\"max_candidates\": 64",
        "\"max_candidates\": 513",
    );
    assert!(matches!(
        WorkerManifest::from_json(&candidates),
        Err(ManifestError::InvalidManifest)
    ));

    for (field, valid, invalid) in [
        ("max_retries", "1", "9"),
        ("breaker_failure_threshold", "3", "0"),
        ("breaker_cooldown_micros", "5000000", "0"),
    ] {
        let unbounded = replace_field(
            &fixture.manifest,
            &format!("\"{field}\": {valid}"),
            &format!("\"{field}\": {invalid}"),
        );
        assert!(matches!(
            WorkerManifest::from_json(&unbounded),
            Err(ManifestError::InvalidManifest)
        ));
    }

    let distribution = replace_field(
        &fixture.manifest,
        "operator_provided_only",
        "redistributable",
    );
    assert!(matches!(
        WorkerManifest::from_json(&distribution),
        Err(ManifestError::InvalidManifest)
    ));
}

#[test]
fn unsupported_platform_and_non_https_license_are_refused_without_artifact_bytes() {
    let fixture = Fixture::new();
    let platform = replace_field(
        &fixture.manifest,
        pgcontext_worker::current_platform_id(),
        "unsupported-platform",
    );
    assert!(matches!(
        WorkerManifest::from_json(&platform),
        Err(ManifestError::InvalidManifest)
    ));

    let unsupported = replace_field(
        &fixture.manifest,
        pgcontext_worker::current_platform_id(),
        "unsupported",
    );
    assert!(matches!(
        WorkerManifest::from_json(&unsupported),
        Err(ManifestError::InvalidManifest)
    ));

    let license = replace_field(
        &fixture.manifest,
        "https://www.apache.org/licenses/LICENSE-2.0",
        "http://example.invalid/license",
    );
    assert!(matches!(
        WorkerManifest::from_json(&license),
        Err(ManifestError::InvalidManifest)
    ));
}

#[test]
fn artifact_loader_requires_a_directory_not_an_untrusted_file_base() {
    let fixture = Fixture::new();
    let manifest = fixture.parse();
    assert!(matches!(
        VerifiedArtifact::load(&manifest, Path::new("/definitely/missing/pgcontext-worker")),
        Err(ManifestError::ArtifactUnavailable)
    ));
}
