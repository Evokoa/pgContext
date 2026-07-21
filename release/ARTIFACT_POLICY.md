# V1 Artifact Verification Policy

pgContext V1 release artifacts are unsigned. Maintainers and users must verify
the published SHA-256 checksums and the immutable source commit recorded in
`PROVENANCE.json` before installing an artifact.

`SBOM.spdx.json` inventories the Rust dependency graph used by the source
candidate. The OCI image carries BuildKit provenance separately and must be
selected by its published manifest digest rather than a mutable convenience
tag when reproducibility matters.

Artifact signing and signature verification infrastructure are post-V1 roadmap
work. The absence of a signature must not be represented as signed or verified.
