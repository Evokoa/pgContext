# Release Tooling

This is the index for pgContext release tooling. The scripts build and inspect
local release candidates; none of them signs, uploads, publishes, pushes, or
creates a GitHub release.

Detailed policy and evidence requirements live in the
[release process](../docs/contributor_guide/release_process.md) and
[release matrix](../docs/contributor_guide/release_matrix.md).

## Prerequisites

- the Rust toolchain pinned by `rust-toolchain.toml`;
- PostgreSQL 17 server development files and its `pg_config`;
- the versions in `tool-versions.env`, including `cargo-pgrx`, `cargo-audit`,
  `cargo-deny`, and gitleaks;
- Docker with Compose v2 for configuration and playground validation;
- standard Unix build tools, Git, `jq`, and `shasum`.

`tool-versions.env` is the release-tool version source of truth. CI and local
release entry points must source it rather than selecting the latest tool.

## Layout

- `checks/open-source-readiness.sh` runs the bounded clean-source gate.
- `build-packages.sh` creates and verifies the complete V1 source payload.
- `ARTIFACT_POLICY.md` defines the unsigned V1 verification boundary.
- `docker/Dockerfile` builds PostgreSQL 17 with pgContext installed.
- `docker/compose.yml` runs the disposable playground.
- `../scripts/` contains focused contract checks and evidence-report runners.
- `../scripts/check-public-docs.py` validates the selected GitHub Markdown
  navigation, local links/anchors, and deterministic content manifest.
- `../scripts/render-release-notes.py` binds the exact candidate SHA into the
  final notes used by the protected publication job.
- `../.github/workflows/ci.yml` runs bounded pull-request checks.
- `../.github/workflows/release-gates.yml` manually runs expensive release
  certification.

## Check a source candidate

Run from a clean commit with PostgreSQL 17 selected:

```sh
PG_CONFIG=/path/to/postgresql-17/bin/pg_config \
  release/checks/open-source-readiness.sh
```

During development, `--allow-dirty` exercises the same gate and records dirty
provenance in the source-package smoke. It cannot approve a release candidate;
the final invocation must use the clean default.

The gate scans complete Git history for secrets using the narrow sentinel and
historical fake-fixture allowlist in `.gitleaks.toml`; checks formatting, clippy,
tests, rustdoc, PG17 extension compilation, crate boundaries, generated public
contracts, documentation, unsafe comments, shell syntax, Docker Compose, and a
reproducible source payload. Its package smoke writes to a unique
`target/source-readiness-package.*` evidence directory.

This bounded gate does not certify crash recovery, long fuzz campaigns,
performance/recall thresholds, Linux sanitizers, multiple PostgreSQL majors,
binary reproducibility, signing, or publication. Those remain separate release
matrix gates.

## Run the Docker playground

```sh
scripts/quickstart.sh
scripts/quickstart.sh psql
scripts/quickstart.sh clean
```

The first command builds the local image, starts PostgreSQL, installs pgContext,
and runs `playground/demo.sql`. `clean` removes the disposable container and
volume. The prebuilt `pg17-v0.2.0` registry tag becomes live only after the
protected publication workflow completes.

## Prove both install paths

From a clean candidate with the local pgrx PostgreSQL 17 instance running on
port 28817, run:

```sh
scripts/run-install-report.sh \
  --pg-config /path/to/postgresql-17/bin/pg_config
```

The report builds and unpacks the source archive outside the checkout, installs
it, exercises extension removal and recreation, runs the source and Docker
HNSW/filter demos, checks Docker health and cleanup, and verifies unsupported
install targets fail clearly. SHA-named logs and the report are written under
`target/install-gates/`.

## Package a clean commit

Validate the tag-shaped release version and PGXN metadata before packaging:

```sh
scripts/validate-release.py --tag v0.2.0
scripts/build-pgxn-dist.sh v0.2.0
scripts/render-homebrew-formula.sh \
  --archive dist/pgContext-0.2.0.zip \
  --out-dir target/homebrew-formula
```

The formula renderer pins the release-asset URL and SHA-256 checksum and stages
both `pgcontext.rb` and its versioned `pgrx@0.19.1.rb` build dependency for the
external `Evokoa/homebrew-tap` repository. It does not modify that repository.

Build the local, merged amd64/arm64 OCI release candidate with provenance:

```sh
scripts/build-release-image.sh v0.2.0
```

The command writes a SHA-named OCI archive and BuildKit metadata under
`target/release-images/`. Verify the healthcheck, PostgreSQL major, packaged
demo, exact filtered results, and HNSW plan on both included platforms:

```sh
scripts/verify-release-image.sh OCI_ARCHIVE IMAGE linux/amd64
scripts/verify-release-image.sh OCI_ARCHIVE IMAGE linux/arm64
```

Neither command pushes an image or changes registry tags. A diagnostic
`--allow-dirty` build is visibly marked `-dirty` in its revision label and
filename and cannot be mistaken for clean candidate evidence.

After the prepared and SHA tags exist in GHCR, the publish workflow uses
`scripts/promote-release-image.sh` to require that both resolve to the accepted
manifest digest before applying the six PG17/version-friendly tags. Its
`--plan` mode is read-only; tag promotion itself is reserved for the protected
publish job.

The manual `.github/workflows/release.yml` accepts only `prepare` or
`publish`, an exact `vX.Y.Z` tag, and its full candidate SHA. Prepare builds,
tests, and uploads the PGXN, Homebrew, and multi-architecture OCI inputs without
changing a public registry. Publish names that prepare run and its reviewed
manifest digest, revalidates the unchanged artifacts behind the protected
`release` environment, then publishes GHCR, PGXN, the GitHub release asset, and
the external Homebrew tap in dependency order.

```sh
release/build-packages.sh v0.2.0
```

Artifacts are written under ignored `dist/` by default. The script refuses a
dirty worktree or a non-empty output directory, builds the exact PGXN/GitHub
source archive twice, and requires the archives to be byte-identical. It stages
`SHA256SUMS`, `PROVENANCE.json`, an SPDX SBOM, Apache-2.0 license/notice, and the
V1 artifact policy, then verifies the complete payload.

V1 artifacts are intentionally unsigned. Verify `SHA256SUMS`, the immutable
candidate SHA in `PROVENANCE.json`, and the OCI manifest digest before use.
Signing is post-V1 roadmap work.

Signing, package/image publication, release tags, and hosted uploads require
separate maintainer authorization. The current open work also includes Linux
ASan/TSan certification and the longer hardening campaigns described by the
release matrix.

## Verify public documentation

The repository uses GitHub Markdown as the source renderer. Navigation lives in
`docs/navigation.json`; `docs/site-manifest.json` binds every public page and
its local links to deterministic content hashes.

```sh
scripts/check-public-docs.py --check
tests/shell/check_public_docs_smoke.sh
scripts/render-release-notes.py \
  --candidate-sha "$(git rev-parse HEAD)" \
  --output target/release-notes.md
```

Use `scripts/check-public-docs.py --write` only after intentionally changing
public documentation or navigation, then review the manifest diff.

Release notes are editorial content. Review them directly for accuracy,
clarity, feature maturity, limitations, and roadmap boundaries before running
the renderer; CI does not enforce headings or exact marketing phrases.
