#!/usr/bin/env python3
"""Validate release-tag, Cargo, control, SQL, and PGXN metadata agreement."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
TAG_RE = re.compile(r"^v(?P<version>(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*))$")


def fail(message: str) -> None:
    print(f"release validation failed: {message}", file=sys.stderr)
    raise SystemExit(1)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True, help="Release tag in vX.Y.Z form")
    parser.add_argument(
        "--check-master",
        action="store_true",
        help="Require the release tag's commit to be contained in origin/master",
    )
    return parser.parse_args()


def control_version() -> str:
    match = re.search(
        r"^default_version\s*=\s*'([^']+)'\s*$",
        (ROOT / "pgcontext.control").read_text(encoding="utf-8"),
        re.MULTILINE,
    )
    if match is None:
        fail("pgcontext.control has no default_version")
    return match.group(1)


def require_equal(label: str, actual: object, expected: object) -> None:
    if actual != expected:
        fail(f"{label} {actual!r} does not match {expected!r}")


def run_git(*args: str) -> str:
    try:
        return subprocess.check_output(
            ["git", *args],
            cwd=ROOT,
            text=True,
            stderr=subprocess.STDOUT,
        ).strip()
    except subprocess.CalledProcessError as error:
        fail(f"git {' '.join(args)} failed: {error.output.strip()}")


def toml_value(path: Path, section: str, key: str) -> object:
    text = path.read_text(encoding="utf-8")
    section_match = re.search(
        rf"^\[{re.escape(section)}\]\s*$\n(?P<body>.*?)(?=^\[|\Z)",
        text,
        re.MULTILINE | re.DOTALL,
    )
    if section_match is None:
        fail(f"{path.relative_to(ROOT)} has no [{section}] section")
    value_match = re.search(
        rf"^{re.escape(key)}\s*=\s*(?P<value>.+?)\s*$",
        section_match.group("body"),
        re.MULTILINE,
    )
    if value_match is None:
        fail(f"{path.relative_to(ROOT)} [{section}] has no {key}")
    try:
        return json.loads(value_match.group("value"))
    except json.JSONDecodeError as error:
        fail(f"{path.relative_to(ROOT)} {key} is not a supported TOML literal: {error}")


def main() -> None:
    args = parse_args()
    match = TAG_RE.fullmatch(args.tag)
    if match is None:
        fail(f"tag must use vX.Y.Z form, got {args.tag!r}")
    version = match.group("version")

    meta = json.loads((ROOT / "META.json").read_text(encoding="utf-8"))
    workspace_toml = ROOT / "Cargo.toml"
    package_toml = ROOT / "crates/context-pg/Cargo.toml"

    require_equal(
        "context-pg version",
        toml_value(package_toml, "package", "version"),
        version,
    )
    require_equal("control version", control_version(), version)
    require_equal("META.json version", meta.get("version"), version)
    require_equal(
        "META.json provides.pgcontext.version",
        meta.get("provides", {}).get("pgcontext", {}).get("version"),
        version,
    )
    require_equal(
        "META.json provides.pgcontext.file",
        meta.get("provides", {}).get("pgcontext", {}).get("file"),
        "pgcontext.control",
    )
    require_equal(
        "META.json provides.pgcontext_pgvector.version",
        meta.get("provides", {}).get("pgcontext_pgvector", {}).get("version"),
        version,
    )
    require_equal(
        "META.json provides.pgcontext_pgvector.file",
        meta.get("provides", {}).get("pgcontext_pgvector", {}).get("file"),
        "pgcontext_pgvector.control",
    )
    if not (ROOT / f"sql/pgcontext--{version}.sql").is_file():
        fail(f"generated SQL sql/pgcontext--{version}.sql is missing")
    if not (ROOT / f"sql/pgcontext_pgvector--{version}.sql").is_file():
        fail(f"bridge SQL sql/pgcontext_pgvector--{version}.sql is missing")

    require_equal("META.json name", meta.get("name"), "pgContext")
    require_equal("META.json license", meta.get("license"), "apache_2_0")
    require_equal(
        "Cargo license",
        toml_value(workspace_toml, "workspace.package", "license"),
        "Apache-2.0",
    )
    require_equal(
        "supported PostgreSQL majors",
        toml_value(
            workspace_toml,
            "workspace.metadata.pgcontext",
            "supported-postgres-versions",
        ),
        ["17"],
    )
    require_equal(
        "repository URL",
        meta.get("resources", {}).get("repository", {}).get("url"),
        "https://github.com/evokoa/pgcontext.git",
    )
    require_equal(
        "issues URL",
        meta.get("resources", {}).get("bugtracker", {}).get("web"),
        "https://github.com/evokoa/pgcontext/issues",
    )
    require_equal(
        "documentation URL",
        meta.get("resources", {}).get("documentation"),
        "https://github.com/evokoa/pgcontext/tree/master/docs",
    )
    if "Evokoa Team <team@evokoa.com>" not in meta.get("maintainer", []):
        fail("META.json must name Evokoa Team <team@evokoa.com>")
    required_tags = {"vector", "hnsw", "postgresql", "retrieval", "rust", "pgrx"}
    missing_tags = sorted(required_tags - set(meta.get("tags", [])))
    if missing_tags:
        fail(f"META.json is missing tags: {', '.join(missing_tags)}")

    if args.check_master:
        run_git("fetch", "--no-tags", "origin", "master:refs/remotes/origin/master")
        tag_sha = run_git("rev-list", "-n", "1", args.tag)
        containing = run_git("branch", "-r", "--contains", tag_sha).split()
        if "origin/master" not in containing:
            fail(f"{args.tag} commit {tag_sha} is not contained in origin/master")

    print(f"release validation passed for {args.tag} (PostgreSQL 17)")


if __name__ == "__main__":
    main()
