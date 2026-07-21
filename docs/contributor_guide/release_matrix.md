# Release Gate Matrix

V1 supports PostgreSQL 17. PostgreSQL 15, 16, and 18 are post-V1 certification
targets, and PostgreSQL 14 is legacy best-effort; none is a V1 release blocker.

| PostgreSQL | Status | Required release gates |
|---:|---|---|
| 14 | Best-effort legacy | May run warning-only compatibility checks; failures do not block release. |
| 15 | First-class planned | Fast Rust tests, `context-pg` check, pgrx SQL tests, heavy lifecycle tests. |
| 16 | First-class planned | Fast Rust tests, `context-pg` check, pgrx SQL tests, heavy lifecycle tests. |
| 17 | Primary first-class planned | Fast Rust tests, `context-pg` check, pgrx SQL tests, heavy lifecycle tests, local smoke gates. |
| 18 | First-class planned | Fast Rust tests, `context-pg` check, pgrx SQL tests, heavy lifecycle tests. |

## Gate Tiers

Pull requests run format, clippy, fast workspace tests, docs, source hygiene,
and supply-chain checks. They also check `context-pg` for supported PostgreSQL
features when the CI runner can provide matching PostgreSQL development files.

Final supply-chain evidence is captured with:

```sh
scripts/run-supply-chain-report.sh \
  --out-dir target/supply-chain/release-candidate
```

The report stays incomplete for dirty trees, dry-runs, advisory failures, or
license/ban/source failures from `cargo deny check`.

Nightly jobs run fuzz smoke checks, Miri for storage, sanitizer jobs where the
platform supports them, generated SQL reproducibility checks, and optional
coverage or unused-dependency checks.

Future multi-major certification jobs run the full PostgreSQL 15-18 matrix:

- `cargo test --workspace --exclude context-pg --all-features`;
- `cargo check -p context-pg --no-default-features --features pgXX`;
- `cargo pgrx test --release -p context-pg pgXX`;
- `cargo pgrx schema -p context-pg pgXX --out target/release-sql/pgXX.sql`;
- heavy install, backup/restore, crash/restart, upgrade, low-memory, and
  corruption tests for each supported PostgreSQL major;
- `cargo audit`, `cargo deny check`, source hygiene, docs, license review, and
  benchmark/recall reports.

Generated extension SQL must be built from a clean tree with Rust `1.96.0`,
pgrx `0.19.1`, and the same release feature set. Release notes must state any
waived benchmark, fuzz, sanitizer, Miri, platform, or PostgreSQL-version gate.

Use the local matrix runner to capture auditable fast, pgrx, and heavy gate
evidence:

```sh
scripts/run-postgres-matrix-gates.sh --allow-missing
```

Use `--mode fast`, `--mode schema`, `--mode pgrx`, or `--mode heavy` to rerun
one gate tier. Fast mode records separate rows for workspace fast tests,
`context-pg` feature checks, and `context-pg` Rust tests. Schema mode records
generated extension SQL paths and checksums. Heavy mode records one report row
per heavy harness script, including install, upgrade, backup/restore,
cross-version import, crash/restart, VACUUM, concurrency, recall, ACL/RLS,
late-interaction ANN serving, low-memory, corruption, and SQLSTATE gates.

`--allow-missing` records unsupported local hosts as skipped when a matching
`pg_config` is unavailable. Do not use skipped or failed rows as release
approval; rerun the missing PostgreSQL majors on CI or a release host with the
matching development files installed before checking off the matrix gate.
Matrix reports mark approval as `incomplete` whenever any row is skipped,
failed, missing, or dry-run.
Before the first released upgrade fixture exists, the `heavy:upgrade_matrix`
row records current-version lifecycle coverage and is marked skipped for
upgrade-from-previous evidence. Do not use that skipped row to check off upgrade
coverage without an explicit release waiver.

The release-gates workflow also runs the all-major report directly with
`scripts/run-postgres-matrix-gates.sh --mode all --out-dir
target/postgres-matrix/all-postgres` and uploads it as
`combined-postgres-matrix-report`. That report is the CI artifact shaped for
final release-note validation: it must contain every fast, schema, pgrx, and
heavy row for PostgreSQL 15, 16, 17, and 18 with no skipped, missing, failed, or
dry-run rows before the PostgreSQL matrix gate can close.

## Containerized Linux Gate

Run the containerized Linux gate before publishing release artifacts:

```sh
PG_MAJOR=17 scripts/release-linux-container-gates.sh
```

Repeat with `PG_MAJOR=15`, `16`, `17`, and `18` for the supported matrix. The
container pins Rust and pgrx versions, installs matching PostgreSQL development
files, runs fast Rust gates, checks `context-pg`, and writes generated extension
SQL under `target/release-sql/`. It does not replace `cargo pgrx test`; use the
PostgreSQL matrix runner for pgrx SQL-test evidence.

Before claiming macOS and Linux build coverage, attach a combined platform build
report:

```sh
scripts/run-platform-build-report.sh \
  --platform macos \
  --out-dir target/platform-builds/macos

scripts/run-platform-build-report.sh \
  --platform linux \
  --out-dir target/platform-builds/linux

scripts/run-platform-build-report.sh \
  --merge-report target/platform-builds/macos/report.md \
  --merge-report target/platform-builds/linux/report.md \
  --out-dir target/platform-builds/release-candidate
```

Run each per-platform report on the matching release host, then merge those
reports into the release-candidate directory. The Linux container gate is
diagnostic evidence only. Final platform approval requires a clean tree, macOS
and Linux rows, all platform-build CI gates passing, no dry-run/skipped rows,
Linux host evidence from a Linux runner, macOS host evidence from a Darwin
runner, and the generated `summary.tsv` referenced from the merged report.
The release-gates workflow uploads each per-host `target/platform-builds`
directory and has a `platform-build-summary` job that downloads the Linux and
macOS reports, runs the same merge command, and uploads
`combined-platform-build-report` from `target/platform-builds/release-candidate`.

## Release Artifact Report

The current V1 publication payload is PG17-only and unsigned. Build it twice,
verify its complete checksum manifest, SBOM, provenance, and policy:

```sh
release/build-packages.sh --out-dir target/release-payload v0.1.0
scripts/verify-release-payload.py \
  --tag v0.1.0 \
  --candidate-sha "$(git rev-parse HEAD)" \
  target/release-payload
```

The older combined report below is retained for post-V1 multi-major and signing
certification. Pass one generated SQL artifact for every first-class major and any
package/install outputs. Each generated SQL artifact must have a sibling
`pgXX.sql.build.log` from the `cargo pgrx schema` command that records the
command, commit, artifact path, and SHA-256 hash. The report records commit SHA,
clean-tree state, Rust, Cargo, cargo-pgrx, artifact sizes, SHA-256 checksums,
signature verification status, and version consistency between `context-pg` and
`pgcontext.control`. Reports stay `incomplete` for dirty trees, dry-runs,
missing or empty artifacts, toolchain/version mismatches, missing PostgreSQL
15-18 generated SQL artifacts, missing or invalid generation logs, duplicate
generated SQL artifacts for the same major, and unsigned or unverifiable
artifacts when signatures are required. Per-major CI artifact reports are useful
diagnostics, but the final release-artifact approval requires the combined
all-major report. The release-gates workflow uploads each generated
`target/release-sql/pgXX.sql` artifact and build log, downloads the four
per-major artifacts into one directory, and emits a combined unsigned diagnostic
report at `target/release-artifacts/all-postgres`. Publishing-host release
approval still requires rerunning the combined report with signatures when the
publishing target requires signed artifacts.
