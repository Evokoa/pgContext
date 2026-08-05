# pgContext User Guide

pgContext is an open-source PostgreSQL extension for AI vector and hybrid
retrieval.

This guide distinguishes stable, implemented behavior from experimental and
planned paths. pgContext 0.2.0 targets PostgreSQL 17 and 18.

## Current Status

The [Supported Features](supported_features.md) page is the canonical inventory
of currently implemented behavior and its maturity. It is checked against the
code-backed capability contract. Planned-only work remains in the
[post-V1 product roadmap](roadmap.md).

## Implemented Core Behavior

- [Supported features](supported_features.md)
- [Installation](installation.md)
- [Configuration](configuration.md)
- [Packaged playground](playground.md)
- [Collections](collections.md)
- [Production quickstart](quickstart.md)
- [SQL API contract](api_reference.md)
- [Dense vectors and exact search](vector_search.md)
- [Multi-tenancy runbook](multi_tenancy.md)
- [Client-facing examples](client_examples.md)
- [Filters](filters.md)
- [Hybrid retrieval](hybrid_retrieval.md)
- [Lexical and fuzzy retrieval](lexical_retrieval.md)
- [Adaptive-dimension retrieval](adaptive_dimension.md)
- [Retrieval methods overview](retrieval_methods.md)
- [Indexes](indexes.md)
- [Rebuildable storage artifacts](storage.md)
- [Operations and support](operations.md)
- [Troubleshooting and maintenance runbook](troubleshooting.md)
- [Known limitations](limitations.md)
- [pgvector and Qdrant parity matrix](parity_matrix.md)
- [Exact metric and operator matrix](metric_operator_matrix.md)
- [Metric definitions and edge-case semantics](metric_semantics.md)
- [Post-V1 product roadmap](roadmap.md)
- [Installed SQL object and option inventory](sql_object_inventory.md)
- [Migrating from pgvector](pgvector_migration.md)
- [Support, version, upgrade, and deprecation policy](support_policy.md)
- [Rollback and repair plan](rollback.md)
- [First production release notes](release_notes.md)
- [Error categories and SQLSTATEs](errors.md)
- [Security model](security.md)

## PostgreSQL Support

PostgreSQL 17 and 18 are supported V1 release targets. OCI images are built
and runtime-verified for linux/amd64 and linux/arm64; PostgreSQL 17 remains the
primary benchmark and deep-lifecycle qualification target.
