//! Supervised single-request worker runtime.

use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use context_query::RerankRequest;

use crate::{LinearPairV1, RerankBackendError, WireRerankRequest};

/// Stable failure returned by the worker composition root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerRunError {
    /// The request was not valid bounded wire data.
    InvalidRequest,
    /// The request was already expired.
    Expired,
    /// The selected adapter exceeded its bounded deadline.
    Timeout,
    /// The provider is unavailable or the failure circuit is open.
    Unavailable,
    /// The provider process or supervised task failed.
    Crash,
    /// The provider produced an incomplete operational response.
    PartialOutput,
    /// The caller cancelled the request or worker shutdown began.
    Cancelled,
    /// The persistent worker has opened its bounded failure circuit.
    CircuitOpen,
}

impl WorkerRunError {
    /// Returns a content-free diagnostic suitable for stderr or telemetry.
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::Expired => "expired",
            Self::Timeout => "timeout",
            Self::Unavailable => "unavailable",
            Self::Crash => "crash",
            Self::PartialOutput => "partial_output",
            Self::Cancelled => "cancelled",
            Self::CircuitOpen => "circuit_open",
        }
    }

    /// Returns the exact SQL failure reason for an operational failure.
    ///
    /// Invalid and cancelled work cannot be finalized as fallback output.
    #[must_use]
    pub const fn finalization_failure_reason(self) -> Option<&'static str> {
        match self {
            Self::Expired => Some("expired"),
            Self::Timeout => Some("timeout"),
            Self::Unavailable | Self::CircuitOpen => Some("unavailable"),
            Self::Crash => Some("crash"),
            Self::PartialOutput => Some("partial_output"),
            Self::InvalidRequest | Self::Cancelled => None,
        }
    }

    const fn is_retryable(self) -> bool {
        matches!(self, Self::Unavailable | Self::Crash | Self::PartialOutput)
    }

    const fn is_circuit_failure(self) -> bool {
        matches!(
            self,
            Self::Timeout | Self::Unavailable | Self::Crash | Self::PartialOutput
        )
    }
}

/// Immutable retry and circuit-breaker policy loaded from a verified manifest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkerPolicy {
    max_retries: usize,
    failure_threshold: usize,
    cooldown_micros: u64,
}

impl WorkerPolicy {
    /// Copies the already-validated resilience policy from a manifest.
    #[must_use]
    pub const fn from_manifest(manifest: &crate::WorkerManifest) -> Self {
        Self {
            max_retries: manifest.max_retries(),
            failure_threshold: manifest.breaker_failure_threshold(),
            cooldown_micros: manifest.breaker_cooldown_micros(),
        }
    }
}

/// Persistent bounded retry and circuit-breaker state for worker attempts.
#[derive(Debug)]
pub struct WorkerSupervisor {
    policy: WorkerPolicy,
    consecutive_failures: usize,
    opened_at_micros: Option<u64>,
}

impl WorkerSupervisor {
    /// Creates closed supervisor state for one immutable policy.
    #[must_use]
    pub const fn new(policy: WorkerPolicy) -> Self {
        Self {
            policy,
            consecutive_failures: 0,
            opened_at_micros: None,
        }
    }

    /// Executes a fallible async attempt under retry and breaker policy.
    ///
    /// The callback is invoked at most `1 + max_retries` times. Invalid and
    /// expired inputs are caller failures, so they neither retry nor open the
    /// backend circuit.
    ///
    /// # Errors
    ///
    /// Returns [`WorkerRunError::CircuitOpen`] during cooldown, or the final
    /// attempt's stable failure after bounded retries.
    pub async fn run<T, F, Fut>(&mut self, mut attempt: F) -> Result<T, WorkerRunError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, WorkerRunError>>,
    {
        let now = unix_micros()?;
        if let Some(opened_at) = self.opened_at_micros {
            let reopen_at = opened_at.saturating_add(self.policy.cooldown_micros);
            if now < reopen_at {
                return Err(WorkerRunError::CircuitOpen);
            }
            self.opened_at_micros = None;
            self.consecutive_failures = 0;
        }

        for attempt_index in 0..=self.policy.max_retries {
            match attempt().await {
                Ok(value) => {
                    self.consecutive_failures = 0;
                    self.opened_at_micros = None;
                    return Ok(value);
                }
                Err(error) if !error.is_circuit_failure() => {
                    return Err(error);
                }
                Err(error) if error.is_retryable() && attempt_index < self.policy.max_retries => {}
                Err(error) => {
                    self.consecutive_failures = self.consecutive_failures.saturating_add(1);
                    if self.consecutive_failures >= self.policy.failure_threshold {
                        self.opened_at_micros = Some(unix_micros()?);
                    }
                    return Err(error);
                }
            }
        }
        unreachable!("inclusive bounded retry loop always returns")
    }
}

impl core::fmt::Display for WorkerRunError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.stable_name())
    }
}

impl std::error::Error for WorkerRunError {}

/// Validates and scores one bounded JSON request on a tracked CPU task.
///
/// The caller owns transport I/O. This function owns the async deadline and
/// never detaches the blocking scorer: on timeout it requests cooperative
/// cancellation and joins the task before returning.
///
/// # Errors
///
/// Returns [`WorkerRunError`] for malformed/expired input, timeout, adapter
/// refusal, serialization failure, or a task join failure.
pub async fn score_one(backend: LinearPairV1, payload: String) -> Result<String, WorkerRunError> {
    let wire =
        WireRerankRequest::from_json(&payload).map_err(|_| WorkerRunError::InvalidRequest)?;
    drop(payload);
    let request = wire
        .into_request()
        .map_err(|_| WorkerRunError::InvalidRequest)?;
    score_request(backend, request).await
}

/// Scores one already-validated request on a tracked CPU task.
///
/// This is the retry-friendly entry point: callers parse wire data once, then
/// clone only the bounded validated request for another attempt.
///
/// # Errors
///
/// Returns [`WorkerRunError`] for expiry, timeout, adapter refusal,
/// serialization failure, or a task join failure.
pub async fn score_request(
    backend: LinearPairV1,
    request: RerankRequest,
) -> Result<String, WorkerRunError> {
    let now = unix_micros()?;
    let expires_at = request
        .expires_at_micros()
        .checked_sub(now)
        .filter(|remaining| *remaining > 0)
        .map(|_| request.expires_at_micros())
        .ok_or(WorkerRunError::Expired)?;
    let deadline = now
        .saturating_add(backend.max_elapsed_micros())
        .min(expires_at);
    score_request_until(backend, request, deadline, Arc::new(AtomicBool::new(false))).await
}

/// Scores a request with all retries sharing one absolute elapsed deadline.
///
/// # Errors
///
/// Returns a typed operational, caller-invalid, expiry, or cancellation error.
pub async fn score_with_supervisor(
    supervisor: &mut WorkerSupervisor,
    backend: LinearPairV1,
    request: RerankRequest,
    cancelled: Arc<AtomicBool>,
) -> Result<String, WorkerRunError> {
    let now = unix_micros()?;
    if request.expires_at_micros() <= now {
        return Err(WorkerRunError::Expired);
    }
    let deadline = now
        .saturating_add(backend.max_elapsed_micros())
        .min(request.expires_at_micros());
    supervisor
        .run(|| {
            score_request_until(
                backend.clone(),
                request.clone(),
                deadline,
                Arc::clone(&cancelled),
            )
        })
        .await
}

async fn score_request_until(
    backend: LinearPairV1,
    request: RerankRequest,
    deadline_micros: u64,
    cancelled: Arc<AtomicBool>,
) -> Result<String, WorkerRunError> {
    if cancelled.load(Ordering::Relaxed) {
        return Err(WorkerRunError::Cancelled);
    }
    let now = unix_micros()?;
    if now >= request.expires_at_micros() {
        return Err(WorkerRunError::Expired);
    }
    let allowed = deadline_micros
        .checked_sub(now)
        .filter(|remaining| *remaining > 0);
    let Some(allowed) = allowed else {
        return Err(if deadline_micros == request.expires_at_micros() {
            WorkerRunError::Expired
        } else {
            WorkerRunError::Timeout
        });
    };
    let scorer_shutdown = Arc::clone(&cancelled);
    let attempt_cancelled = Arc::new(AtomicBool::new(false));
    let scorer_attempt_cancelled = Arc::clone(&attempt_cancelled);
    let expires_at_micros = request.expires_at_micros();
    let mut task = tokio::task::spawn_blocking(move || {
        debug_test_score_delay(&scorer_shutdown, &scorer_attempt_cancelled)?;
        let response = backend
            .score_with_cancel(&request, &scorer_shutdown, &scorer_attempt_cancelled)
            .map_err(map_backend_error)?;
        response
            .to_json()
            .map_err(|_| WorkerRunError::InvalidRequest)
    });
    match tokio::time::timeout(Duration::from_micros(allowed), &mut task).await {
        Ok(joined) => joined.map_err(|_| WorkerRunError::Crash)?,
        Err(_) => {
            attempt_cancelled.store(true, Ordering::Relaxed);
            let _ = task.await.map_err(|_| WorkerRunError::Crash)?;
            Err(if deadline_micros == expires_at_micros {
                WorkerRunError::Expired
            } else {
                WorkerRunError::Timeout
            })
        }
    }
}

#[cfg(all(feature = "worker-test-hooks", debug_assertions))]
fn debug_test_score_delay(
    shutdown: &AtomicBool,
    attempt_cancelled: &AtomicBool,
) -> Result<(), WorkerRunError> {
    let Ok(raw_delay) = std::env::var("PGCONTEXT_WORKER_TEST_SCORE_DELAY_MICROS") else {
        return Ok(());
    };
    let delay = raw_delay
        .parse::<u64>()
        .map_err(|_| WorkerRunError::InvalidRequest)?;
    if let Ok(marker) = std::env::var("PGCONTEXT_WORKER_TEST_SCORE_STARTED") {
        std::fs::write(marker, b"started").map_err(|_| WorkerRunError::Crash)?;
    }
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_micros(delay) {
        if shutdown.load(Ordering::Relaxed) || attempt_cancelled.load(Ordering::Relaxed) {
            return Err(WorkerRunError::Cancelled);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

#[cfg(not(all(feature = "worker-test-hooks", debug_assertions)))]
fn debug_test_score_delay(
    _shutdown: &AtomicBool,
    _attempt_cancelled: &AtomicBool,
) -> Result<(), WorkerRunError> {
    Ok(())
}

fn map_backend_error(error: RerankBackendError) -> WorkerRunError {
    match error {
        RerankBackendError::Unavailable => WorkerRunError::Unavailable,
        RerankBackendError::Timeout => WorkerRunError::Timeout,
        RerankBackendError::Cancelled => WorkerRunError::Cancelled,
        RerankBackendError::Transport { .. } => WorkerRunError::Crash,
        RerankBackendError::Rejected(context_query::RerankRejection::Incomplete) => {
            WorkerRunError::PartialOutput
        }
        RerankBackendError::Rejected(context_query::RerankRejection::Expired) => {
            WorkerRunError::Expired
        }
        RerankBackendError::InvalidPlan { .. }
        | RerankBackendError::Wire(_)
        | RerankBackendError::Rejected(_) => WorkerRunError::InvalidRequest,
    }
}

fn unix_micros() -> Result<u64, WorkerRunError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| WorkerRunError::Expired)?;
    u64::try_from(elapsed.as_micros()).map_err(|_| WorkerRunError::Expired)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    const fn policy(max_retries: usize, failure_threshold: usize) -> WorkerPolicy {
        WorkerPolicy {
            max_retries,
            failure_threshold,
            cooldown_micros: 60_000_000,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn retries_are_bounded_and_a_success_resets_failure_state() {
        let mut supervisor = WorkerSupervisor::new(policy(1, 2));
        let mut calls = 0_usize;
        let value = supervisor
            .run(|| {
                calls += 1;
                async move {
                    if calls == 1 {
                        Err(WorkerRunError::Unavailable)
                    } else {
                        Ok(7)
                    }
                }
            })
            .await
            .expect("the one frozen retry should succeed");
        assert_eq!(value, 7);
        assert_eq!(calls, 2);

        let error = supervisor
            .run(|| async { Err::<(), _>(WorkerRunError::Unavailable) })
            .await
            .expect_err("a later backend failure should remain visible");
        assert_eq!(error, WorkerRunError::Unavailable);
        assert_eq!(supervisor.consecutive_failures, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn consecutive_failures_open_the_circuit_without_another_attempt() {
        let mut supervisor = WorkerSupervisor::new(policy(0, 2));
        let mut calls = 0_usize;
        for _ in 0..2 {
            let error = supervisor
                .run(|| {
                    calls += 1;
                    async { Err::<(), _>(WorkerRunError::Timeout) }
                })
                .await
                .expect_err("backend failure should be visible");
            assert_eq!(error, WorkerRunError::Timeout);
        }
        let error = supervisor
            .run(|| {
                calls += 1;
                async { Ok(()) }
            })
            .await
            .expect_err("open circuit should reject before the callback");
        assert_eq!(error, WorkerRunError::CircuitOpen);
        assert_eq!(calls, 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invalid_requests_do_not_retry_or_poison_the_backend_circuit() {
        let mut supervisor = WorkerSupervisor::new(policy(3, 1));
        let mut calls = 0_usize;
        let error = supervisor
            .run(|| {
                calls += 1;
                async { Err::<(), _>(WorkerRunError::InvalidRequest) }
            })
            .await
            .expect_err("invalid request should fail");
        assert_eq!(error, WorkerRunError::InvalidRequest);
        assert_eq!(calls, 1);
        assert_eq!(supervisor.consecutive_failures, 0);
        assert_eq!(supervisor.opened_at_micros, None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn expiry_reached_between_retries_does_not_poison_the_circuit() {
        let mut supervisor = WorkerSupervisor::new(policy(2, 1));
        let mut calls = 0_usize;
        let error = supervisor
            .run(|| {
                calls += 1;
                async move {
                    if calls == 1 {
                        Err::<(), _>(WorkerRunError::Unavailable)
                    } else {
                        Err::<(), _>(WorkerRunError::Expired)
                    }
                }
            })
            .await
            .expect_err("expiry should stop the retry sequence");
        assert_eq!(error, WorkerRunError::Expired);
        assert_eq!(calls, 2);
        assert_eq!(supervisor.consecutive_failures, 0);
        assert_eq!(supervisor.opened_at_micros, None);
    }
}
