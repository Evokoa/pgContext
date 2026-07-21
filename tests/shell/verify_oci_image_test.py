#!/usr/bin/env python3
"""Tamper-negative tests for the OCI release-image verifier."""

from __future__ import annotations

import hashlib
import io
import json
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
VERIFY = ROOT / "scripts" / "verify-oci-image.py"
IMAGE = "ghcr.io/evokoa/pgcontext:pg17-v0.1.0-prepared"
REVISION = "a" * 40
MEDIA_MANIFEST = "application/vnd.oci.image.manifest.v1+json"
MEDIA_INDEX = "application/vnd.oci.image.index.v1+json"


class Fixture:
    def __init__(self) -> None:
        self.blobs: dict[str, bytes] = {}

    def add(self, value: object, media_type: str) -> dict[str, object]:
        payload = json.dumps(value, separators=(",", ":"), sort_keys=True).encode()
        return self.add_bytes(payload, media_type)

    def add_bytes(self, payload: bytes, media_type: str) -> dict[str, object]:
        digest = hashlib.sha256(payload).hexdigest()
        self.blobs[digest] = payload
        return {"mediaType": media_type, "digest": f"sha256:{digest}", "size": len(payload)}

    def write(
        self,
        path: Path,
        *,
        omit: str | None = None,
        corrupt: str | None = None,
        attestation_mode: str | None = None,
    ) -> None:
        image_descriptors = []
        attestation_descriptors = []
        labels = {
            "org.opencontainers.image.licenses": "Apache-2.0",
            "org.opencontainers.image.postgresql.major": "17",
            "org.opencontainers.image.revision": REVISION,
            "org.opencontainers.image.source": "https://github.com/evokoa/pgcontext",
            "org.opencontainers.image.version": "0.1.0",
        }
        for architecture in ("amd64", "arm64"):
            config = self.add({"config": {"Labels": labels}}, "application/vnd.oci.image.config.v1+json")
            layer = self.add_bytes(f"layer-{architecture}".encode(), "application/vnd.oci.image.layer.v1.tar")
            manifest = self.add(
                {"schemaVersion": 2, "mediaType": MEDIA_MANIFEST, "config": config, "layers": [layer]},
                MEDIA_MANIFEST,
            )
            manifest["platform"] = {"os": "linux", "architecture": architecture}
            image_descriptors.append(manifest)

            attested_manifest = (
                image_descriptors[0]
                if attestation_mode == "duplicate" and architecture == "arm64"
                else manifest
            )
            manifest_hash = str(attested_manifest["digest"]).split(":", 1)[1]
            predicate_type = (
                "https://example.invalid/provenance"
                if attestation_mode == "predicate" and architecture == "amd64"
                else "https://slsa.dev/provenance/v0.2"
            )
            statement = self.add(
                {
                    "_type": "https://in-toto.io/Statement/v0.1",
                    "predicateType": predicate_type,
                    "subject": [{"name": architecture, "digest": {"sha256": manifest_hash}}],
                    "predicate": {},
                },
                "application/vnd.in-toto+json",
            )
            statement["annotations"] = {
                "in-toto.io/predicate-type": "https://slsa.dev/provenance/v0.2"
            }
            attestation_config = self.add({}, "application/vnd.oci.image.config.v1+json")
            attestation = self.add(
                {
                    "schemaVersion": 2,
                    "mediaType": MEDIA_MANIFEST,
                    "config": attestation_config,
                    "layers": [statement],
                },
                MEDIA_MANIFEST,
            )
            attestation["platform"] = {"os": "unknown", "architecture": "unknown"}
            attestation["annotations"] = {
                "vnd.docker.reference.type": "attestation-manifest",
                "vnd.docker.reference.digest": attested_manifest["digest"],
            }
            if not (attestation_mode == "missing" and architecture == "arm64"):
                attestation_descriptors.append(attestation)

        index = self.add(
            {
                "schemaVersion": 2,
                "mediaType": MEDIA_INDEX,
                "manifests": image_descriptors + attestation_descriptors,
            },
            MEDIA_INDEX,
        )
        index["annotations"] = {"io.containerd.image.name": IMAGE}
        root = {"schemaVersion": 2, "mediaType": MEDIA_INDEX, "manifests": [index]}

        with tarfile.open(path, "w") as archive:
            self._member(archive, "index.json", json.dumps(root).encode())
            self._member(archive, "oci-layout", b'{"imageLayoutVersion":"1.0.0"}')
            for digest, payload in self.blobs.items():
                if digest == omit:
                    continue
                if digest == corrupt:
                    payload += b"tampered"
                self._member(archive, f"blobs/sha256/{digest}", payload)

    @staticmethod
    def _member(archive: tarfile.TarFile, name: str, payload: bytes) -> None:
        info = tarfile.TarInfo(name)
        info.size = len(payload)
        archive.addfile(info, io.BytesIO(payload))


class VerifyOciImageTest(unittest.TestCase):
    def run_verifier(self, archive: Path) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(VERIFY), "--image", IMAGE, "--version", "0.1.0", "--revision", REVISION, str(archive)],
            check=False,
            text=True,
            capture_output=True,
        )

    def test_accepts_complete_fixture(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / "image.tar"
            Fixture().write(archive)
            self.assertEqual(self.run_verifier(archive).returncode, 0)

    def test_rejects_missing_and_corrupt_blobs(self) -> None:
        for mutation in ("omit", "corrupt"):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as directory:
                fixture = Fixture()
                archive = Path(directory) / "image.tar"
                layer_digest = hashlib.sha256(b"layer-amd64").hexdigest()
                fixture.write(archive, **{mutation: layer_digest})
                result = self.run_verifier(archive)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("OCI image verification failed", result.stderr)

    def test_rejects_invalid_attestations(self) -> None:
        for mode in ("duplicate", "missing", "predicate"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory:
                archive = Path(directory) / "image.tar"
                Fixture().write(archive, attestation_mode=mode)
                result = self.run_verifier(archive)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("OCI image verification failed", result.stderr)


if __name__ == "__main__":
    unittest.main()
