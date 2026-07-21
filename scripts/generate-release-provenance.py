#!/usr/bin/env python3
"""Write deterministic provenance for a staged pgContext source payload."""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--archive", required=True, type=Path)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--source-date-epoch", required=True, type=int)
    parser.add_argument("--dirty", action="store_true")
    parser.add_argument("--postgres", default="source-only")
    return parser.parse_args()


def command_version(*command: str) -> str:
    return subprocess.check_output(command, cwd=ROOT, text=True).strip()


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main() -> None:
    args = parse_args()
    toolchain = ROOT / "rust-toolchain.toml"
    release_tools = ROOT / "release/tool-versions.env"
    provenance = {
        "project": "pgcontext",
        "repository": "https://github.com/evokoa/pgcontext",
        "version": args.version,
        "tag": args.tag,
        "commit": args.commit,
        "dirty": args.dirty,
        "source_date_epoch": args.source_date_epoch,
        "reproducible_source": True,
        "signed": False,
        "archive": {
            "name": args.archive.name,
            "sha256": sha256(args.archive),
        },
        "toolchain": {
            "rustc": command_version("rustc", "--version", "--verbose"),
            "cargo": command_version("cargo", "--version"),
            "postgres": args.postgres,
            "rust_toolchain_sha256": sha256(toolchain),
            "release_tool_versions_sha256": sha256(release_tools),
            "release_tool_versions": release_tools.read_text().splitlines(),
        },
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(provenance, indent=2, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
