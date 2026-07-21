#!/usr/bin/env python3
"""Validate public Markdown navigation, links, anchors, and deterministic drift."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path
from urllib.parse import unquote, urlsplit


ROOT = Path(__file__).resolve().parent.parent
MARKDOWN_LINK_RE = re.compile(r"!?\[[^]]*\]\((?P<target>[^) ]+)(?:\s+[^)]*)?\)")
HTML_LINK_RE = re.compile(r"(?:href|src)=[\"'](?P<target>[^\"']+)[\"']")
HEADING_RE = re.compile(r"^#{1,6}\s+(?P<title>.+?)\s*#*\s*$", re.MULTILINE)
FORBIDDEN_PARTS = {".agents", ".codex", "planning", "private", "reference"}


def fail(message: str) -> None:
    print(f"public docs check failed: {message}", file=sys.stderr)
    raise SystemExit(1)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--docs-root", type=Path, default=ROOT / "docs")
    parser.add_argument("--navigation", type=Path)
    parser.add_argument("--manifest", type=Path)
    parser.add_argument(
        "--public-file",
        action="append",
        type=Path,
        default=[],
        help="Additional public Markdown file whose links and hash are checked",
    )
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--write", action="store_true", help="Write the deterministic manifest")
    mode.add_argument("--check", action="store_true", help="Check the deterministic manifest")
    return parser.parse_args()


def slug(title: str) -> str:
    value = re.sub(r"<[^>]+>", "", title)
    value = value.replace("`", "").strip().lower()
    value = re.sub(r"[^\w\- ]", "", value)
    return re.sub(r"[\s_]+", "-", value).strip("-")


def anchors(path: Path) -> set[str]:
    counts: dict[str, int] = {}
    result: set[str] = set()
    for match in HEADING_RE.finditer(path.read_text()):
        base = slug(match.group("title"))
        count = counts.get(base, 0)
        counts[base] = count + 1
        result.add(base if count == 0 else f"{base}-{count}")
    return result


def validate_link(source: Path, target: str, project_root: Path) -> None:
    split = urlsplit(target)
    if split.scheme or target.startswith("//"):
        return
    decoded_path = unquote(split.path)
    if not decoded_path:
        destination = source
    elif decoded_path.startswith("/"):
        fail(f"absolute local link in {source}: {target}")
    else:
        destination = (source.parent / decoded_path).resolve()
    try:
        relative = destination.relative_to(project_root.resolve())
    except ValueError:
        fail(f"link escapes the repository in {source}: {target}")
    if FORBIDDEN_PARTS.intersection(relative.parts):
        fail(f"link exposes a private path in {source}: {target}")
    if not destination.exists():
        fail(f"broken local link in {source}: {target}")
    if split.fragment and destination.suffix.lower() == ".md":
        expected = unquote(split.fragment).lower()
        if expected not in anchors(destination):
            fail(f"broken Markdown anchor in {source}: {target}")


def main() -> None:
    args = parse_args()
    docs_root = args.docs_root.resolve()
    project_root = docs_root.parent
    navigation_path = (args.navigation or docs_root / "navigation.json").resolve()
    manifest_path = (args.manifest or docs_root / "site-manifest.json").resolve()
    navigation = json.loads(navigation_path.read_text())
    if navigation.get("version") != 1 or navigation.get("renderer") != "github-markdown":
        fail("navigation must select version 1 of the GitHub Markdown renderer")

    nav_paths = [
        page["path"]
        for section in navigation.get("sections", [])
        for page in section.get("pages", [])
    ]
    if len(nav_paths) != len(set(nav_paths)):
        fail("navigation contains duplicate pages")
    actual_paths = sorted(
        str(path.relative_to(docs_root)) for path in docs_root.rglob("*.md")
    )
    if sorted(nav_paths) != actual_paths:
        missing = sorted(set(actual_paths) - set(nav_paths))
        unknown = sorted(set(nav_paths) - set(actual_paths))
        fail(f"navigation drift (orphaned={missing}, missing={unknown})")

    linked_assets: set[Path] = set()

    def record(path: Path, relative: str) -> dict[str, object]:
        text = path.read_text()
        if not text.startswith("# ") and "<h1" not in text[:1000]:
            fail(f"page has no level-one title: {relative}")
        targets = [match.group("target") for match in MARKDOWN_LINK_RE.finditer(text)]
        targets.extend(match.group("target") for match in HTML_LINK_RE.finditer(text))
        for target in targets:
            validate_link(path, target, project_root)
            split = urlsplit(target)
            if not split.scheme and split.path:
                destination = (path.parent / unquote(split.path)).resolve()
                if destination.is_file() and destination.suffix.lower() != ".md":
                    linked_assets.add(destination)
        return {
            "path": relative,
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            "links": sorted(targets),
        }

    records = [record(docs_root / relative, relative) for relative in actual_paths]
    public_records = []
    public_files = [path.resolve() for path in args.public_file]
    if not public_files and docs_root == (ROOT / "docs").resolve():
        public_files = [
            ROOT / "README.md",
            ROOT / "CONTRIBUTING.md",
            ROOT / "SECURITY.md",
            ROOT / "CODE_OF_CONDUCT.md",
            ROOT / "release/README.md",
        ]
    for path in public_files:
        try:
            relative = str(path.relative_to(project_root))
        except ValueError:
            fail(f"public file escapes the repository: {path}")
        public_records.append(record(path, relative))

    manifest = {
        "version": 1,
        "renderer": navigation["renderer"],
        "navigation_sha256": hashlib.sha256(navigation_path.read_bytes()).hexdigest(),
        "pages": records,
        "public_files": public_records,
        "linked_assets": [
            {
                "path": str(path.relative_to(project_root)),
                "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            }
            for path in sorted(linked_assets)
        ],
    }
    rendered = json.dumps(manifest, indent=2, sort_keys=True) + "\n"
    if args.write:
        manifest_path.write_text(rendered)
    elif not manifest_path.is_file() or manifest_path.read_text() != rendered:
        fail("site manifest is stale; run scripts/check-public-docs.py --write")
    print(
        f"public docs check passed: {len(records)} pages, "
        f"{len(public_records)} public files"
    )


if __name__ == "__main__":
    main()
