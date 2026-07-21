# Support, Version, Upgrade, and Deprecation Policy

This policy describes the PostgreSQL 17 V1 contract for prototypes, evaluation,
and controlled pilots. Broader production certification is post-V1 roadmap work.

## Supported PostgreSQL Versions

PostgreSQL 17 is the only supported V1 major. Support for it is validated across
compilation, generated SQL, install, backup/restore, WAL/restart, HNSW
lifecycle, filtered ANN, ACL/RLS, partition behavior, both advertised install
paths, PGXN, and multi-architecture GHCR.
PostgreSQL 15, 16, and 18 are planned targets only after their post-V1
version-specific and platform gates; PostgreSQL 14 is legacy best-effort.

## Extension Versions

Stable SQL behavior follows semantic extension-version compatibility after the
first production release. Patch releases may add optional functions, fields,
enum values, diagnostics, or stricter validation for previously invalid input.
They must not remove or repurpose stable SQL objects, change stable result-column
meaning, or change documented SQLSTATEs for the same failure class.

Breaking SQL changes require a new extension version, upgrade notes, and a
named migration or rebuild procedure. A breaking change includes removing a
stable function, changing a stable signature, changing a stable status value,
or making previously valid stable input fail without an explicit migration path.

## Upgrades

Extension update scripts must not scan user data, start index builds, or mutate
user-owned source tables. Upgrades may update pgContext-owned catalogs and SQL
objects, then operators should run smoke checks for collection metadata, exact
search, filters, telemetry, and deployed index paths.

When an upgrade cannot complete safely, the release notes must name the rollback
or repair path. If acceleration artifacts become incompatible, PostgreSQL source
tables remain authoritative and artifacts must be rebuilt or migrated through a
documented procedure.

## Deprecation

Deprecated stable APIs remain callable for at least one minor release after a
replacement is documented. Deprecation notices belong in this guide, release
notes, and upgrade notes. Clients should branch on stable SQLSTATEs and typed
status fields, not on free-form PostgreSQL messages.

Experimental and internal APIs are excluded from the compatibility promise.
`pgcontext_hnsw` and the variant vector wrappers remain experimental even when
implemented and tested in V1; planned serving surfaces live in the public
roadmap and are not implied by their metadata helpers.
