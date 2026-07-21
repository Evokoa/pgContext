#!/usr/bin/env python3
"""Verify platforms, labels, digests, and attestations in an OCI image archive."""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
import tarfile
from pathlib import PurePosixPath


REQUIRED_PLATFORMS = {("linux", "amd64"), ("linux", "arm64")}
INDEX_TYPES = {
    "application/vnd.docker.distribution.manifest.list.v2+json",
    "application/vnd.oci.image.index.v1+json",
}


def fail(message: str) -> None:
    print(f"OCI image verification failed: {message}", file=sys.stderr)
    raise SystemExit(1)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("archive")
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    with tarfile.open(args.archive, "r") as archive:
        members = {member.name: member for member in archive.getmembers()}
        for member in members.values():
            path = PurePosixPath(member.name)
            if member.issym() or member.islnk() or ".." in path.parts:
                fail(f"unsafe archive member: {member.name}")

        def read_json(name: str) -> object:
            member = members.get(name)
            if member is None or not member.isfile():
                fail(f"missing OCI object: {name}")
            source = archive.extractfile(member)
            if source is None:
                fail(f"cannot read OCI object: {name}")
            return json.loads(source.read())

        def blob_payload(descriptor: dict[str, object]) -> bytes:
            digest = str(descriptor.get("digest", ""))
            algorithm, separator, value = digest.partition(":")
            if algorithm != "sha256" or separator != ":" or len(value) != 64:
                fail(f"invalid descriptor digest: {digest}")
            name = f"blobs/sha256/{value}"
            member = members.get(name)
            if member is None:
                fail(f"missing descriptor blob: {digest}")
            source = archive.extractfile(member)
            if source is None:
                fail(f"cannot read descriptor blob: {digest}")
            payload = source.read()
            if descriptor.get("size") != len(payload):
                fail(f"descriptor size mismatch: {digest}")
            if hashlib.sha256(payload).hexdigest() != value:
                fail(f"descriptor digest mismatch: {digest}")
            return payload

        def blob(descriptor: dict[str, object]) -> dict[str, object]:
            document = json.loads(blob_payload(descriptor))
            if not isinstance(document, dict):
                fail(f"descriptor is not a JSON object: {descriptor.get('digest')}")
            return document

        root = read_json("index.json")
        descriptors = list(root.get("manifests", []))
        image_descriptors: list[dict[str, object]] = []
        attestation_descriptors: list[dict[str, object]] = []
        image_names: set[str] = set()
        while descriptors:
            descriptor = descriptors.pop()
            annotations = descriptor.get("annotations", {})
            image_name = annotations.get("io.containerd.image.name")
            if isinstance(image_name, str):
                image_names.add(image_name)
            document = blob(descriptor)
            if descriptor.get("mediaType") in INDEX_TYPES:
                descriptors.extend(document.get("manifests", []))
                continue
            if annotations.get("vnd.docker.reference.type") == "attestation-manifest":
                attestation_descriptors.append(descriptor)
                continue
            image_descriptors.append(descriptor)

        if image_names != {args.image}:
            fail(f"archive image names {sorted(image_names)!r} do not match {args.image!r}")

        platforms = {
            (item.get("platform", {}).get("os"), item.get("platform", {}).get("architecture"))
            for item in image_descriptors
        }
        if platforms != REQUIRED_PLATFORMS:
            fail(f"platforms {sorted(platforms)!r} do not match {sorted(REQUIRED_PLATFORMS)!r}")
        image_digests = {str(item.get("digest")) for item in image_descriptors}
        attestation_subjects: set[str] = set()
        for descriptor in attestation_descriptors:
            annotations = descriptor.get("annotations", {})
            subject_digest = str(annotations.get("vnd.docker.reference.digest", ""))
            if subject_digest not in image_digests or subject_digest in attestation_subjects:
                fail(f"invalid or duplicate attestation subject: {subject_digest}")
            attestation_subjects.add(subject_digest)
            manifest = blob(descriptor)
            blob(manifest["config"])
            layers = manifest.get("layers", [])
            if len(layers) != 1:
                fail(f"attestation {descriptor.get('digest')} must contain one provenance layer")
            layer = layers[0]
            if layer.get("mediaType") != "application/vnd.in-toto+json":
                fail(f"attestation layer has unexpected media type: {layer.get('mediaType')}")
            statement = json.loads(blob_payload(layer))
            if statement.get("_type") != "https://in-toto.io/Statement/v0.1":
                fail("attestation is not an in-toto statement")
            if statement.get("predicateType") != "https://slsa.dev/provenance/v0.2":
                fail("attestation is not SLSA provenance")
            subjects = statement.get("subject", [])
            subject_hashes = {
                f"sha256:{subject.get('digest', {}).get('sha256', '')}"
                for subject in subjects
                if isinstance(subject, dict)
            }
            if subject_hashes != {subject_digest}:
                fail(f"provenance subject does not match {subject_digest}")
        if attestation_subjects != image_digests:
            fail("each platform manifest must have exactly one provenance attestation")

        expected_labels = {
            "org.opencontainers.image.licenses": "Apache-2.0",
            "org.opencontainers.image.postgresql.major": "17",
            "org.opencontainers.image.revision": args.revision,
            "org.opencontainers.image.source": "https://github.com/evokoa/pgcontext",
            "org.opencontainers.image.version": args.version,
        }
        for descriptor in image_descriptors:
            manifest = blob(descriptor)
            config = blob(manifest["config"])
            layers = manifest.get("layers", [])
            if not layers:
                fail(f"image manifest has no layers: {descriptor.get('digest')}")
            for layer in layers:
                blob_payload(layer)
            labels = config.get("config", {}).get("Labels", {})
            for key, expected in expected_labels.items():
                if labels.get(key) != expected:
                    fail(f"label {key} is {labels.get(key)!r}, expected {expected!r}")

    print(f"OCI image verification passed: {args.archive}")


if __name__ == "__main__":
    main()
