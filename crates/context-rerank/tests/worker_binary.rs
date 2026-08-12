//! End-to-end tests for the persistent worker composition root.

#![allow(clippy::expect_used)]

use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use context_core::{OccurrenceId, PointId, SourceVersion};
use context_query::{
    RERANK_CONTENT_DIGEST_BYTES, RerankCandidate, RerankContentDigest, RerankContribution,
    RerankModelName, RerankQuery, RerankRequest, RerankRequestId,
};
use pgcontext_worker::{WireRerankFailure, WireRerankRequest, WireRerankResponse};
use sha2::{Digest, Sha256};

#[cfg(feature = "worker-test-hooks")]
use std::time::{Duration, Instant};

static TEMP_ID: AtomicU64 = AtomicU64::new(1);

struct Fixture {
    root: PathBuf,
    manifest: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        Self::with_runtime(1_000_000, 3)
    }

    fn with_runtime(max_elapsed_micros: u64, breaker_failure_threshold: usize) -> Self {
        let root = std::env::temp_dir().join(format!(
            "pgcontext-worker-bin-{}-{}",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("fixture directory");
        let mut artifact = b"PGLPAIR1".to_vec();
        for weight in [-2.0_f64, 6.0, 1.0, 0.0] {
            artifact.extend_from_slice(&weight.to_le_bytes());
        }
        fs::write(root.join("weights.bin"), &artifact).expect("artifact");
        let digest = Sha256::digest(&artifact)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let manifest = root.join("manifest.json");
        fs::write(
            &manifest,
            format!(
                r#"{{
                  "schema_version":1,
                  "adapter":"linear_pair_v1",
                  "model":"fixture-reranker",
                  "model_revision":7,
                  "artifact_path":"weights.bin",
                  "artifact_bytes":{},
                  "artifact_sha256":"{digest}",
                  "tokenizer_revision":"ascii_tokens_v1",
                  "input_contract":"rerank_envelope_v3",
                  "output_contract":"rerank_response_v3",
                  "failure_contract":"rerank_failure_v1",
                  "score_contract":"higher_is_better_unit_interval",
                  "max_request_bytes":4194304,
                  "max_candidates":64,
                  "max_query_tokens":128,
                  "max_document_tokens":512,
                  "max_elapsed_micros":{max_elapsed_micros},
                  "max_retries":1,
                  "breaker_failure_threshold":{breaker_failure_threshold},
                  "breaker_cooldown_micros":5000000,
                  "supported_platforms":["{}"],
                  "license_spdx":"Apache-2.0",
                  "license_url":"https://www.apache.org/licenses/LICENSE-2.0",
                  "distribution":"operator_provided_only"
                }}"#,
                artifact.len(),
                pgcontext_worker::current_platform_id()
            ),
        )
        .expect("manifest");
        Self { root, manifest }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn request(text: &str) -> String {
    let expires_at_micros = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_micros(),
    )
    .expect("micros")
    .checked_add(5_000_000)
    .expect("expiry");
    let request = RerankRequest::new(
        RerankRequestId::new(1).expect("request"),
        RerankModelName::new("fixture-reranker").expect("model"),
        7,
        expires_at_micros,
        RerankQuery::new("postgres retrieval").expect("query"),
        vec![
            RerankCandidate::new(
                OccurrenceId::new(1).expect("occurrence"),
                PointId::new(1),
                SourceVersion::new(1).expect("version"),
                RerankContentDigest::new([1; RERANK_CONTENT_DIGEST_BYTES]),
                text,
                1,
                0.1,
                vec![RerankContribution::new("fixture", 1, 0.1, 1.0, 0.01).expect("contribution")],
                Vec::new(),
            )
            .expect("candidate"),
        ],
    )
    .expect("request");
    WireRerankRequest::from_request(&request)
        .to_json()
        .expect("wire json")
}

#[test]
fn binary_scores_one_bounded_request_without_logging_authorized_text() {
    let fixture = Fixture::new();
    let sentinel = "SENSITIVE_AUTHORIZED_TEXT postgres retrieval";
    let mut child = Command::new(env!("CARGO_BIN_EXE_pgcontext-worker"))
        .args(["score", "--manifest"])
        .arg(&fixture.manifest)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("worker process");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(request(sentinel).as_bytes())
        .expect("request write");
    let output = child.wait_with_output().expect("worker output");
    assert!(output.status.success());
    let response =
        WireRerankResponse::from_json(std::str::from_utf8(&output.stdout).expect("response utf8"))
            .expect("response");
    assert_eq!(response.request_id, 1);
    assert_eq!(response.scores.len(), 1);
    assert!(!String::from_utf8_lossy(&output.stderr).contains(sentinel));
}

#[test]
fn binary_rejects_bad_usage_and_malformed_input_with_content_free_errors() {
    let usage = Command::new(env!("CARGO_BIN_EXE_pgcontext-worker"))
        .output()
        .expect("usage output");
    assert_eq!(usage.status.code(), Some(64));

    let fixture = Fixture::new();
    let sentinel = "SECRET_SHOULD_NOT_BE_LOGGED";
    let mut child = Command::new(env!("CARGO_BIN_EXE_pgcontext-worker"))
        .args(["score", "--manifest"])
        .arg(&fixture.manifest)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("worker process");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(format!(r#"{{"query":"{sentinel}"}}"#).as_bytes())
        .expect("request write");
    let output = child.wait_with_output().expect("worker output");
    assert_eq!(output.status.code(), Some(65));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid_request"));
    assert!(!stderr.contains(sentinel));
}

#[test]
fn binary_rejects_an_oversized_frame_without_waiting_for_a_newline() {
    let fixture = Fixture::new();
    let mut child = Command::new(env!("CARGO_BIN_EXE_pgcontext-worker"))
        .args(["score", "--manifest"])
        .arg(&fixture.manifest)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("worker process");
    let oversized = vec![b'x'; context_query::MAX_RERANK_WIRE_BYTES + 2];
    let _ = child.stdin.as_mut().expect("stdin").write_all(&oversized);
    let output = child.wait_with_output().expect("worker output");
    assert_eq!(output.status.code(), Some(65));
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_request"));
}

#[test]
fn binary_accepts_a_cwd_relative_manifest_and_multiple_requests() {
    let fixture = Fixture::new();
    let mut child = Command::new(env!("CARGO_BIN_EXE_pgcontext-worker"))
        .current_dir(&fixture.root)
        .args(["score", "--manifest", "manifest.json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("worker process");
    let input = format!("{}\n{}\n", request("first"), request("second"));
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("requests");
    let output = child.wait_with_output().expect("worker output");
    assert!(output.status.success());
    let responses = String::from_utf8(output.stdout).expect("response utf8");
    assert_eq!(responses.lines().count(), 2);
    for response in responses.lines() {
        WireRerankResponse::from_json(response).expect("valid response frame");
    }
}

#[test]
fn binary_keeps_breaker_state_across_operational_failures() {
    let fixture = Fixture::with_runtime(1, 2);
    let mut child = Command::new(env!("CARGO_BIN_EXE_pgcontext-worker"))
        .args(["score", "--manifest"])
        .arg(&fixture.manifest)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("worker process");
    let large_text = "postgres ".repeat(3_000);
    let input = format!(
        "{}\n{}\n{}\n",
        request(&large_text),
        request(&large_text),
        request(&large_text)
    );
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("requests");
    let output = child.wait_with_output().expect("worker output");
    assert!(output.status.success());
    let frames = String::from_utf8(output.stdout).expect("failure frames");
    let failures = frames
        .lines()
        .map(|line| WireRerankFailure::from_json(line).expect("failure frame"))
        .collect::<Vec<_>>();
    assert_eq!(failures.len(), 3);
    assert_eq!(failures[0].request_id, 1);
    assert_eq!(failures[0].failure_reason, "timeout");
    assert_eq!(failures[1].request_id, 1);
    assert_eq!(failures[1].failure_reason, "timeout");
    assert_eq!(failures[2].request_id, 1);
    assert_eq!(failures[2].error, "circuit_open");
    assert_eq!(failures[2].failure_reason, "unavailable");
}

#[test]
#[cfg(feature = "worker-test-hooks")]
fn binary_shutdown_control_cancels_and_joins_active_scoring() {
    let fixture = Fixture::new();
    let started_marker = fixture.root.join("score-started");
    let mut child = Command::new(env!("CARGO_BIN_EXE_pgcontext-worker"))
        .args(["score", "--manifest"])
        .arg(&fixture.manifest)
        .env("PGCONTEXT_WORKER_TEST_SCORE_DELAY_MICROS", "5000000")
        .env("PGCONTEXT_WORKER_TEST_SCORE_STARTED", &started_marker)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("worker process");
    let sentinel = "SHUTDOWN_SENTINEL ".repeat(1_500);
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(format!("{}\n", request(&sentinel)).as_bytes())
        .expect("request");
    let started_wait = Instant::now();
    while !started_marker.exists() && started_wait.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        started_marker.exists(),
        "scoring task must be active before shutdown"
    );
    let shutdown_started = Instant::now();
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(b"shutdown\n")
        .expect("shutdown");
    let output = child.wait_with_output().expect("worker output");
    assert!(output.status.success());
    assert!(shutdown_started.elapsed() < Duration::from_secs(2));
    assert!(
        output.stdout.is_empty(),
        "cancelled scoring must emit no response"
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("SHUTDOWN_SENTINEL"));
}
