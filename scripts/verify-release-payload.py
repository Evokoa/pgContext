#!/usr/bin/env python3
"""Verify a staged pgContext V1 source release payload."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path, PurePosixPath


ROOT = Path(__file__).resolve().parent.parent
SHA256_RE = re.compile(r"^(?P<digest>[0-9a-f]{64})  (?P<name>[^/]+)$")
COMMIT_RE = re.compile(r"^[0-9a-f]{40}$")
SECRET_PATTERNS = (
    b"-----BEGIN " + b"PRIVATE KEY-----",
    b"-----BEGIN RSA " + b"PRIVATE KEY-----",
    b"-----BEGIN OPENSSH " + b"PRIVATE KEY-----",
)
ALLOWED_BINARY_SIGNATURES = {
    "assets/pgcontext-banner.png": b"\x89PNG\r\n\x1a\n",
}


def fail(message: str) -> None:
    print(f"release payload verification failed: {message}", file=sys.stderr)
    raise SystemExit(1)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--candidate-sha", required=True)
    parser.add_argument("--allow-dirty", action="store_true")
    parser.add_argument("payload", type=Path)
    return parser.parse_args()


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main() -> None:
    args = parse_args()
    if not re.fullmatch(r"v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)", args.tag):
        fail("tag must use vX.Y.Z form")
    if not COMMIT_RE.fullmatch(args.candidate_sha):
        fail("candidate SHA must be a full lowercase Git commit")
    checkout_sha = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True
    ).strip()
    if checkout_sha != args.candidate_sha:
        fail("candidate SHA does not match the verifier checkout")
    version = args.tag.removeprefix("v")
    if args.payload.is_symlink():
        fail("payload directory must not be a symbolic link")
    payload = args.payload.resolve()
    if not payload.is_dir():
        fail(f"payload directory does not exist: {payload}")

    expected = {
        f"pgContext-{version}.zip",
        "ARTIFACT_POLICY.md",
        "LICENSE",
        "NOTICE",
        "PROVENANCE.json",
        "SBOM.spdx.json",
        "SHA256SUMS",
    }
    entries = {path.name for path in payload.iterdir()}
    unexpected = sorted(entries - expected)
    missing = sorted(expected - entries)
    if missing:
        fail(f"required payload files are missing: {', '.join(missing)}")
    if unexpected:
        fail(f"unexpected payload files are present: {', '.join(unexpected)}")
    for path in payload.iterdir():
        if path.is_symlink() or not path.is_file():
            fail(f"payload entry must be a regular file: {path.name}")

    checksums: dict[str, str] = {}
    for line in (payload / "SHA256SUMS").read_text().splitlines():
        match = SHA256_RE.fullmatch(line)
        if match is None or match.group("name") in checksums:
            fail("SHA256SUMS has malformed or duplicate entries")
        checksums[match.group("name")] = match.group("digest")
    checksum_targets = expected - {"SHA256SUMS"}
    if set(checksums) != checksum_targets:
        fail("SHA256SUMS does not cover every payload file exactly once")
    for name, expected_digest in checksums.items():
        if sha256(payload / name) != expected_digest:
            fail(f"SHA-256 mismatch for {name}")
    for name, source in {
        "ARTIFACT_POLICY.md": ROOT / "release/ARTIFACT_POLICY.md",
        "LICENSE": ROOT / "LICENSE",
        "NOTICE": ROOT / "NOTICE",
    }.items():
        if (payload / name).read_bytes() != source.read_bytes():
            fail(f"{name} does not match the candidate checkout")

    provenance = json.loads((payload / "PROVENANCE.json").read_text())
    required_provenance = {
        "project": "pgcontext",
        "repository": "https://github.com/evokoa/pgcontext",
        "version": version,
        "tag": args.tag,
        "commit": args.candidate_sha,
        "dirty": args.allow_dirty,
        "reproducible_source": True,
        "signed": False,
    }
    for key, expected_value in required_provenance.items():
        if provenance.get(key) != expected_value:
            fail(f"PROVENANCE.json {key} does not match the candidate")
    archive = payload / f"pgContext-{version}.zip"
    if provenance.get("archive") != {"name": archive.name, "sha256": sha256(archive)}:
        fail("PROVENANCE.json archive identity does not match the payload")
    with tempfile.TemporaryDirectory(prefix="pgcontext-payload-verify-") as temporary:
        expected_archive = Path(temporary) / archive.name
        subprocess.run(
            [
                "git",
                "archive",
                "--format=zip",
                f"--prefix=pgContext-{version}/",
                f"--output={expected_archive}",
                args.candidate_sha,
            ],
            cwd=ROOT,
            check=True,
        )
        if sha256(archive) != sha256(expected_archive):
            fail("source archive is not the exact candidate Git archive")
    source_date_epoch = int(
        subprocess.check_output(
            ["git", "show", "-s", "--format=%ct", args.candidate_sha],
            cwd=ROOT,
            text=True,
        ).strip()
    )
    if provenance.get("source_date_epoch") != source_date_epoch:
        fail("PROVENANCE.json source date does not match the candidate")
    toolchain = provenance.get("toolchain", {})
    if toolchain.get("rust_toolchain_sha256") != sha256(ROOT / "rust-toolchain.toml"):
        fail("PROVENANCE.json Rust toolchain input does not match the candidate")
    if toolchain.get("release_tool_versions_sha256") != sha256(
        ROOT / "release/tool-versions.env"
    ):
        fail("PROVENANCE.json release tool inputs do not match the candidate")

    sbom = json.loads((payload / "SBOM.spdx.json").read_text())
    if sbom.get("spdxVersion") != "SPDX-2.3" or sbom.get("name") != f"pgContext-{version}":
        fail("SBOM identity or SPDX version is invalid")
    expected_namespace = (
        f"https://github.com/evokoa/pgcontext/releases/download/"
        f"{args.tag}/sbom-{args.candidate_sha}"
    )
    if sbom.get("documentNamespace") != expected_namespace:
        fail("SBOM namespace does not name the candidate SHA")
    if not sbom.get("packages"):
        fail("SBOM contains no packages")
    if not any(
        package.get("name") == "context-pg" and package.get("versionInfo") == version
        for package in sbom["packages"]
    ):
        fail("SBOM does not include the pgContext extension package")

    subprocess.run(
        [str(ROOT / "scripts/verify-pgxn-dist.py"), "--tag", args.tag, str(archive)],
        check=True,
    )
    with zipfile.ZipFile(archive) as package:
        for info in package.infolist():
            if info.is_dir():
                continue
            contents = package.read(info)
            if b"\0" in contents:
                relative = PurePosixPath(info.filename).relative_to(
                    f"pgContext-{version}"
                )
                expected_signature = ALLOWED_BINARY_SIGNATURES.get(relative.as_posix())
                if expected_signature is None:
                    fail(
                        "source archive contains unexpected binary content: "
                        f"{info.filename}"
                    )
                if not contents.startswith(expected_signature):
                    fail(
                        "allowlisted source binary has an unexpected signature: "
                        f"{info.filename}"
                    )
            if any(pattern in contents for pattern in SECRET_PATTERNS):
                fail(f"source archive contains private key material: {info.filename}")

    policy = (payload / "ARTIFACT_POLICY.md").read_text().lower()
    required_policy_terms = (
        "signed annotated tag",
        "sha-256",
        "sigstore",
        "immutable",
    )
    if any(term not in policy for term in required_policy_terms):
        fail("artifact policy does not state the V1 verification boundary")
    print(f"release payload verification passed: {payload}")


if __name__ == "__main__":
    main()
