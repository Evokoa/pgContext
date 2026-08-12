//! Binary protocol tests for the persistent automatic-chunking worker.

use std::{
    io::Write,
    process::{Command, Stdio},
};

use pgcontext_worker::{ChunkWorkerRequestV1, ChunkWorkerResponseV1, WireChunkProfileV1};

#[test]
fn binary_processes_multiple_frames_and_shuts_down_cleanly()
-> Result<(), Box<dyn std::error::Error>> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_pgcontext-chunk-worker"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let request = ChunkWorkerRequestV1 {
        version: "chunk_worker_request_v1".to_owned(),
        request_id: 1,
        document_id: 2,
        source_version: 3,
        source_hash: "0123456789abcdef".to_owned(),
        profile_revision: 4,
        parser: "plain_text_v1".to_owned(),
        tokenizer_revision: "unicode_words_v1".to_owned(),
        expires_at_micros: u64::MAX,
        profile: WireChunkProfileV1 {
            target_tokens: 4,
            max_tokens: 8,
            min_tokens: 1,
            overlap_tokens: 1,
            max_document_bytes: 1024,
            include_structure_context: false,
        },
        source_text: "one two three".to_owned(),
    };
    let payload = request.to_json()?;
    let Some(input) = child.stdin.as_mut() else {
        return Err("chunk worker stdin was not piped".into());
    };
    writeln!(input, "{payload}")?;
    writeln!(input, "{payload}")?;
    writeln!(input, "shutdown")?;
    let output = child.wait_with_output()?;
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    let responses = stdout.lines().collect::<Vec<_>>();
    assert_eq!(responses.len(), 2);
    for response in responses {
        assert!(ChunkWorkerResponseV1::from_json(response).is_ok());
    }
    Ok(())
}
