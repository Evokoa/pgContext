#!/usr/bin/env python3
"""Render final release notes with an exact immutable candidate SHA."""

from __future__ import annotations

import argparse
import posixpath
import re
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
TOKEN = "{{CANDIDATE_SHA}}"
RELATIVE_MARKDOWN_LINK = re.compile(r"\]\((?!https://|mailto:|#)([^)]+\.md(?:#[^)]*)?)\)")


def resolve_repository_links(notes: str, candidate_sha: str) -> str:
    """Resolve documentation links for a GitHub release body at an immutable SHA."""

    def replace(match: re.Match[str]) -> str:
        target = match.group(1)
        path, separator, fragment = target.partition("#")
        repository_path = posixpath.normpath(posixpath.join("docs/user_guide", path))
        suffix = f"#{fragment}" if separator else ""
        return (
            "](https://github.com/evokoa/pgcontext/blob/"
            f"{candidate_sha}/{repository_path}{suffix})"
        )

    return RELATIVE_MARKDOWN_LINK.sub(replace, notes)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--candidate-sha", required=True)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    if re.fullmatch(r"[0-9a-f]{40}", args.candidate_sha) is None:
        parser.error("--candidate-sha must be a full lowercase Git commit")
    template = (ROOT / "docs/user_guide/release_notes.md").read_text()
    if template.count(TOKEN) > 1:
        parser.error("release notes may contain at most one candidate SHA token")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    rendered = resolve_repository_links(
        template.replace(TOKEN, args.candidate_sha), args.candidate_sha
    )
    args.output.write_text(rendered)


if __name__ == "__main__":
    main()
