#!/usr/bin/env python3
"""Generate a deterministic SPDX 2.3 SBOM from Cargo metadata and Cargo.lock."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--version", required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--source-date-epoch", required=True, type=int)
    return parser.parse_args()


def spdx_id(name: str, version: str, source: str) -> str:
    identity = f"{name}\0{version}\0{source}".encode()
    suffix = hashlib.sha256(identity).hexdigest()[:16]
    safe_name = "".join(char if char.isalnum() else "-" for char in name)
    return f"SPDXRef-Package-{safe_name}-{suffix}"


def main() -> None:
    args = parse_args()
    metadata = json.loads(
        subprocess.check_output(
            ["cargo", "metadata", "--locked", "--format-version", "1"],
            cwd=ROOT,
            text=True,
        )
    )
    workspace_ids = set(metadata["workspace_members"])
    package_ids: dict[str, str] = {}
    packages = []
    for package in sorted(
        metadata["packages"], key=lambda value: (value["name"], value["version"], value["id"])
    ):
        source = package.get("source") or ""
        identifier = spdx_id(package["name"], package["version"], source)
        package_ids[package["id"]] = identifier
        record = {
            "SPDXID": identifier,
            "name": package["name"],
            "versionInfo": package["version"],
            "downloadLocation": source or "NOASSERTION",
            "filesAnalyzed": False,
            "licenseConcluded": "NOASSERTION",
            "licenseDeclared": package.get("license") or "NOASSERTION",
            "copyrightText": "NOASSERTION",
        }
        packages.append(record)

    relationships = [
        {
            "spdxElementId": "SPDXRef-DOCUMENT",
            "relationshipType": "DESCRIBES",
            "relatedSpdxElement": package_ids[package_id],
        }
        for package_id in sorted(workspace_ids)
    ]
    for node in sorted(metadata["resolve"]["nodes"], key=lambda value: value["id"]):
        for dependency in sorted(node["dependencies"]):
            relationships.append(
                {
                    "spdxElementId": package_ids[node["id"]],
                    "relationshipType": "DEPENDS_ON",
                    "relatedSpdxElement": package_ids[dependency],
                }
            )

    created = dt.datetime.fromtimestamp(
        args.source_date_epoch, tz=dt.timezone.utc
    ).strftime("%Y-%m-%dT%H:%M:%SZ")
    document = {
        "spdxVersion": "SPDX-2.3",
        "dataLicense": "CC0-1.0",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": f"pgContext-{args.version}",
        "documentNamespace": (
            f"https://github.com/evokoa/pgcontext/releases/download/"
            f"v{args.version}/sbom-{args.commit}"
        ),
        "creationInfo": {
            "created": created,
            "creators": ["Tool: pgcontext-release-sbom"],
        },
        "packages": packages,
        "relationships": relationships,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(document, indent=2, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
