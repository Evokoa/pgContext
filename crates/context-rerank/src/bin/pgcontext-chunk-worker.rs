//! Persistent no-egress composition root for deterministic document chunking.

use std::{
    io::{self, BufRead, Write},
    time::{SystemTime, UNIX_EPOCH},
};

use pgcontext_worker::{
    ChunkWorkerError, ChunkWorkerFailureV1, ChunkWorkerRequestV1, MAX_CHUNK_WORKER_FRAME_BYTES,
    process_chunk_request,
};

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "pgcontext-chunk-worker: {error}");
            std::process::ExitCode::from(65)
        }
    }
}

fn run() -> Result<(), ChunkWorkerError> {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    while let Some(frame) = read_bounded_frame(&mut input)? {
        if frame == "shutdown" {
            return Ok(());
        }
        let parsed = ChunkWorkerRequestV1::from_json(&frame);
        drop(frame);
        let (request_id, result) = match parsed {
            Ok(request) => {
                let request_id = Some(request.request_id);
                (request_id, process_chunk_request(request, now_micros()?))
            }
            Err(error) => (None, Err(error)),
        };
        let response = match result {
            Ok(response) => response.to_json()?,
            Err(error) => ChunkWorkerFailureV1::new(request_id, error).to_json()?,
        };
        output
            .write_all(response.as_bytes())
            .and_then(|()| output.write_all(b"\n"))
            .and_then(|()| output.flush())
            .map_err(|_| ChunkWorkerError::MalformedResponse)?;
    }
    Ok(())
}

fn read_bounded_frame(reader: &mut impl BufRead) -> Result<Option<String>, ChunkWorkerError> {
    let ceiling = MAX_CHUNK_WORKER_FRAME_BYTES
        .checked_add(1)
        .ok_or(ChunkWorkerError::FrameTooLarge)?;
    let mut frame = Vec::with_capacity(ceiling.min(64 * 1024));
    loop {
        let available = reader
            .fill_buf()
            .map_err(|_| ChunkWorkerError::MalformedRequest)?;
        if available.is_empty() {
            if frame.is_empty() {
                return Ok(None);
            }
            break;
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.unwrap_or(available.len());
        let remaining = ceiling.saturating_sub(frame.len());
        let admitted = take.min(remaining);
        frame.extend_from_slice(&available[..admitted]);
        let consumed = newline.map_or(admitted, |position| position.saturating_add(1));
        reader.consume(consumed);
        if admitted < take || (frame.len() == ceiling && newline.is_none()) {
            return Err(ChunkWorkerError::FrameTooLarge);
        }
        if newline.is_some() {
            break;
        }
    }
    if frame.last() == Some(&b'\r') {
        frame.pop();
    }
    if frame.len() > MAX_CHUNK_WORKER_FRAME_BYTES {
        return Err(ChunkWorkerError::FrameTooLarge);
    }
    String::from_utf8(frame)
        .map(Some)
        .map_err(|_| ChunkWorkerError::MalformedRequest)
}

fn now_micros() -> Result<u64, ChunkWorkerError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ChunkWorkerError::MalformedRequest)?;
    u64::try_from(duration.as_micros()).map_err(|_| ChunkWorkerError::MalformedRequest)
}
