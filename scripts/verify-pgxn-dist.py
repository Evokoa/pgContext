#!/usr/bin/env python3
"""Verify a pgContext PGXN archive's prefix, metadata, and public contents."""

from __future__ import annotations

import argparse
import json
import re
import stat
import sys
import zipfile
from pathlib import Path, PurePosixPath


TAG_RE = re.compile(r"^v(?P<version>(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*))$")
FORBIDDEN_PARTS = {
    ".agents",
    ".codex",
    ".git",
    "dist",
    "planning",
    "private",
    "reference",
    "target",
}
MAX_FILE_SIZE = 10 * 1024 * 1024
MAX_ARCHIVE_SIZE = 50 * 1024 * 1024
REPOSITORY = "https://github.com/evokoa/pgcontext"
SECRET_MARKERS = (
    b"-----BEGIN " + b"PRIVATE KEY-----",
    b"-----BEGIN RSA " + b"PRIVATE KEY-----",
    b"-----BEGIN OPENSSH " + b"PRIVATE KEY-----",
)
ALLOWED_BINARY_SIGNATURES = {
    "assets/pgcontext-banner.png": b"\x89PNG\r\n\x1a\n",
}


def fail(message: str) -> None:
    print(f"PGXN archive verification failed: {message}", file=sys.stderr)
    raise SystemExit(1)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True, help="Release tag in vX.Y.Z form")
    parser.add_argument("archive", type=Path)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    match = TAG_RE.fullmatch(args.tag)
    if match is None:
        fail(f"tag must use vX.Y.Z form, got {args.tag!r}")
    version = match.group("version")
    prefix = f"pgContext-{version}/"

    if args.archive.name != f"pgContext-{version}.zip":
        fail(f"archive name {args.archive.name!r} does not match version {version}")
    if not args.archive.is_file():
        fail(f"archive does not exist: {args.archive}")

    with zipfile.ZipFile(args.archive) as package:
        file_entries = [info for info in package.infolist() if not info.is_dir()]
        files = {info.filename: info for info in file_entries}
        if not files:
            fail("archive is empty")
        if len(files) != len(file_entries):
            fail("archive contains duplicate file names")
        if sum(info.file_size for info in file_entries) > MAX_ARCHIVE_SIZE:
            fail("uncompressed archive exceeds the 50 MiB source limit")
        outside = sorted(name for name in files if not name.startswith(prefix))
        if outside:
            fail(f"files escape archive prefix: {', '.join(outside[:3])}")

        required = {
            f"{prefix}Cargo.lock",
            f"{prefix}Cargo.toml",
            f"{prefix}LICENSE",
            f"{prefix}Makefile",
            f"{prefix}META.json",
            f"{prefix}README.md",
            f"{prefix}crates/context-pg/Cargo.toml",
            f"{prefix}pgcontext.control",
            f"{prefix}pgcontext_pgvector.control",
            f"{prefix}sql/pgcontext--{version}.sql",
            f"{prefix}sql/pgcontext_pgvector--{version}.sql",
        }
        missing = sorted(required - files.keys())
        if missing:
            fail(f"required files are missing: {', '.join(missing)}")

        for name, info in files.items():
            relative = PurePosixPath(name.removeprefix(prefix))
            if any(part in {"", ".", ".."} for part in relative.parts):
                fail(f"unsafe relative path is present: {name}")
            if FORBIDDEN_PARTS.intersection(relative.parts):
                fail(f"private or generated path is present: {name}")
            unix_mode = info.external_attr >> 16
            file_type = stat.S_IFMT(unix_mode)
            if file_type == stat.S_IFLNK:
                fail(f"symbolic link is not allowed in PGXN archive: {name}")
            if file_type not in {0, stat.S_IFREG}:
                fail(f"special file is not allowed in PGXN archive: {name}")
            if info.file_size > MAX_FILE_SIZE:
                fail(f"source file exceeds the 10 MiB limit: {name}")
            contents = package.read(name)
            if b"\0" in contents:
                expected_signature = ALLOWED_BINARY_SIGNATURES.get(relative.as_posix())
                if expected_signature is None:
                    fail(f"unexpected binary content is present: {name}")
                if not contents.startswith(expected_signature):
                    fail(f"allowlisted binary has an unexpected signature: {name}")
            if any(marker in contents for marker in SECRET_MARKERS):
                fail(f"private key material is present: {name}")

        meta = json.loads(package.read(f"{prefix}META.json"))
        if meta.get("version") != version:
            fail(f"META.json version {meta.get('version')!r} does not match {version!r}")
        if meta.get("provides", {}).get("pgcontext", {}).get("version") != version:
            fail("META.json provides.pgcontext.version does not match the archive")
        if (
            meta.get("provides", {})
            .get("pgcontext_pgvector", {})
            .get("version")
            != version
        ):
            fail("META.json provides.pgcontext_pgvector.version does not match the archive")
        resources = meta.get("resources", {})
        if resources.get("homepage") != REPOSITORY:
            fail("META.json homepage does not match the pgContext repository")
        if resources.get("repository", {}).get("url") != f"{REPOSITORY}.git":
            fail("META.json repository does not match the pgContext repository")

        workspace = package.read(f"{prefix}Cargo.toml").decode()
        if not re.search(r'^license\s*=\s*"Apache-2\.0"\s*$', workspace, re.MULTILINE):
            fail("Cargo workspace license is not Apache-2.0")
        if not re.search(
            rf'^repository\s*=\s*"{re.escape(REPOSITORY)}"\s*$',
            workspace,
            re.MULTILINE,
        ):
            fail("Cargo workspace repository identity does not match pgContext")
        extension = package.read(f"{prefix}crates/context-pg/Cargo.toml").decode()
        if not re.search(r'^name\s*=\s*"context-pg"\s*$', extension, re.MULTILINE):
            fail("extension Cargo package identity does not match context-pg")
        if not re.search(
            rf'^version\s*=\s*"{re.escape(version)}"\s*$',
            extension,
            re.MULTILINE,
        ):
            fail("extension Cargo package version does not match the archive")

        control = package.read(f"{prefix}pgcontext.control").decode()
        control_match = re.search(
            r"^default_version\s*=\s*['\"](?P<version>[^'\"]+)['\"]\s*$",
            control,
            re.MULTILINE,
        )
        if control_match is None or control_match.group("version") != version:
            fail("control-file version does not match the archive")

        bridge_control = package.read(f"{prefix}pgcontext_pgvector.control").decode()
        bridge_control_match = re.search(
            r"^default_version\s*=\s*['\"](?P<version>[^'\"]+)['\"]\s*$",
            bridge_control,
            re.MULTILINE,
        )
        if (
            bridge_control_match is None
            or bridge_control_match.group("version") != version
        ):
            fail("bridge control-file version does not match the archive")

    print(f"PGXN archive verification passed: {args.archive}")


if __name__ == "__main__":
    main()
