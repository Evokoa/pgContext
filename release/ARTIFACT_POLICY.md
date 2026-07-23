# V1 Artifact Verification Policy

pgContext V1 source files do not carry embedded signatures. The release is
authenticated by a GitHub-verified signed annotated tag, published SHA-256
checksums, and a Sigstore build-provenance attestation for the source archive.
Maintainers and users must also verify the immutable source commit recorded in
`PROVENANCE.json` before installing an artifact.

`SBOM.spdx.json` inventories the Rust dependency graph used by the source
candidate. The OCI image carries BuildKit provenance plus a Sigstore
attestation and must be selected by its published manifest digest rather than a
mutable convenience tag when reproducibility matters.

GitHub release immutability is intentionally disabled. Maintainers must never
replace a published asset or move a published version tag; corrections require
a new release version. Do not describe an individual file as having an embedded
signature; verification applies to the signed tag and build attestations.
