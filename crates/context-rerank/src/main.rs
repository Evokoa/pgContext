//! Composition root for the provider-neutral pgContext reranker worker.

use std::{
    ffi::OsString,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use pgcontext_worker::{
    LinearPairV1, MAX_RERANK_WIRE_BYTES, ManifestError, VerifiedArtifact, WireRerankFailure,
    WireRerankRequest, WorkerManifest, WorkerPolicy, WorkerRunError, WorkerSupervisor,
    score_with_supervisor,
};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};

const EX_USAGE: u8 = 64;
const EX_DATAERR: u8 = 65;
const EX_CONFIG: u8 = 78;

fn main() -> std::process::ExitCode {
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| CliError::Runtime)
        .and_then(|runtime| runtime.block_on(run(std::env::args_os())));
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(std::io::stderr().lock(), "pgcontext-worker: {error}");
            std::process::ExitCode::from(error.exit_code())
        }
    }
}

async fn run(arguments: impl IntoIterator<Item = OsString>) -> Result<(), CliError> {
    let manifest_path = parse_arguments(arguments)?;
    let configuration = tokio::task::spawn_blocking(move || load_backend(&manifest_path))
        .await
        .map_err(|_| CliError::Runtime)??;
    let mut supervisor = WorkerSupervisor::new(configuration.policy);
    let shutdown = Arc::new(AtomicBool::new(false));
    let reader_shutdown = Arc::clone(&shutdown);
    let (input_sender, mut input_receiver) = tokio::sync::mpsc::channel::<String>(2);
    let reader = tokio::spawn(async move {
        let mut input = BufReader::new(tokio::io::stdin());
        while let Some(payload) = read_bounded_frame(&mut input).await? {
            if payload == "shutdown" {
                reader_shutdown.store(true, Ordering::Relaxed);
                break;
            }
            if input_sender.send(payload).await.is_err() {
                break;
            }
        }
        Ok::<(), CliError>(())
    });
    let mut stdout = tokio::io::stdout();
    let mut terminal_error = None;
    while let Some(payload) = input_receiver.recv().await {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        let request_result = WireRerankRequest::from_json(&payload)
            .and_then(WireRerankRequest::into_request)
            .map_err(|_| CliError::Run(WorkerRunError::InvalidRequest));
        drop(payload);
        let request = match request_result {
            Ok(request) => request,
            Err(error) => {
                terminal_error = Some(error);
                break;
            }
        };
        let request_id = request.request_id();
        let response = match score_with_supervisor(
            &mut supervisor,
            configuration.backend.clone(),
            request,
            Arc::clone(&shutdown),
        )
        .await
        {
            Ok(response) => response,
            Err(WorkerRunError::Cancelled) if shutdown.load(Ordering::Relaxed) => break,
            Err(error) if error.finalization_failure_reason().is_some() => {
                let Some(failure) = WireRerankFailure::from_error(request_id, error) else {
                    terminal_error = Some(CliError::Run(error));
                    break;
                };
                let failure = failure
                    .to_json()
                    .map_err(|_| CliError::Run(WorkerRunError::Crash));
                let failure = match failure {
                    Ok(failure) => failure,
                    Err(error) => {
                        terminal_error = Some(error);
                        break;
                    }
                };
                if stdout.write_all(failure.as_bytes()).await.is_err()
                    || stdout.write_all(b"\n").await.is_err()
                    || stdout.flush().await.is_err()
                {
                    terminal_error = Some(CliError::Io);
                    break;
                }
                continue;
            }
            Err(error) => {
                terminal_error = Some(CliError::Run(error));
                break;
            }
        };
        if stdout.write_all(response.as_bytes()).await.is_err()
            || stdout.write_all(b"\n").await.is_err()
            || stdout.flush().await.is_err()
        {
            terminal_error = Some(CliError::Io);
            break;
        }
    }
    if let Some(error) = terminal_error {
        shutdown.store(true, Ordering::Relaxed);
        reader.abort();
        let _ = reader.await;
        return Err(error);
    }
    reader.await.map_err(|_| CliError::Runtime)??;
    Ok(())
}

async fn read_bounded_frame(
    reader: &mut (impl AsyncBufRead + Unpin),
) -> Result<Option<String>, CliError> {
    // One extra byte admits a CRLF terminator without ever growing an input
    // allocation beyond the frozen wire ceiling plus that delimiter byte.
    let frame_ceiling = MAX_RERANK_WIRE_BYTES
        .checked_add(1)
        .ok_or(CliError::Run(WorkerRunError::InvalidRequest))?;
    let mut frame = Vec::with_capacity(frame_ceiling);
    loop {
        let available = reader.fill_buf().await.map_err(|_| CliError::Io)?;
        if available.is_empty() {
            if frame.is_empty() {
                return Ok(None);
            }
            break;
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.unwrap_or(available.len());
        let remaining = frame_ceiling.saturating_sub(frame.len());
        let admitted = take.min(remaining);
        frame.extend_from_slice(&available[..admitted]);
        let consumed = newline.map_or(admitted, |position| position.saturating_add(1));
        reader.consume(consumed);
        if admitted < take || (frame.len() == frame_ceiling && newline.is_none()) {
            return Err(CliError::Run(WorkerRunError::InvalidRequest));
        }
        if newline.is_some() {
            break;
        }
    }
    if frame.last() == Some(&b'\r') {
        frame.pop();
    }
    if frame.len() > MAX_RERANK_WIRE_BYTES {
        return Err(CliError::Run(WorkerRunError::InvalidRequest));
    }
    String::from_utf8(frame)
        .map(Some)
        .map_err(|_| CliError::Run(WorkerRunError::InvalidRequest))
}

fn parse_arguments(arguments: impl IntoIterator<Item = OsString>) -> Result<PathBuf, CliError> {
    let mut arguments = arguments.into_iter();
    let _program = arguments.next();
    match (
        arguments.next(),
        arguments.next(),
        arguments.next(),
        arguments.next(),
    ) {
        (Some(command), Some(flag), Some(path), None)
            if command == "score" && flag == "--manifest" =>
        {
            Ok(PathBuf::from(path))
        }
        _ => Err(CliError::Usage),
    }
}

struct WorkerConfiguration {
    backend: LinearPairV1,
    policy: WorkerPolicy,
}

fn load_backend(manifest_path: &Path) -> Result<WorkerConfiguration, CliError> {
    let manifest = WorkerManifest::load(manifest_path).map_err(CliError::Manifest)?;
    let directory = manifest_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let artifact = VerifiedArtifact::load(&manifest, directory).map_err(CliError::Manifest)?;
    let backend = LinearPairV1::load(&manifest, artifact).map_err(|_| CliError::Configuration)?;
    Ok(WorkerConfiguration {
        backend,
        policy: WorkerPolicy::from_manifest(&manifest),
    })
}

#[derive(Clone, Copy, Debug)]
enum CliError {
    Usage,
    Io,
    Runtime,
    Configuration,
    Manifest(ManifestError),
    Run(WorkerRunError),
}

impl CliError {
    const fn exit_code(self) -> u8 {
        match self {
            Self::Usage => EX_USAGE,
            Self::Run(_) => EX_DATAERR,
            Self::Io | Self::Runtime | Self::Configuration | Self::Manifest(_) => EX_CONFIG,
        }
    }
}

impl core::fmt::Display for CliError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let stable = match self {
            Self::Usage => "usage: score --manifest PATH",
            Self::Io => "io",
            Self::Runtime => "runtime",
            Self::Configuration => "configuration",
            Self::Manifest(error) => error.stable_name(),
            Self::Run(error) => error.stable_name(),
        };
        formatter.write_str(stable)
    }
}
