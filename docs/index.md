# pgContext Documentation

pgContext adds exact vector search, persisted dense HNSW, metadata
filtering, and hybrid retrieval directly to the PostgreSQL tables you
already own. PostgreSQL remains the source of truth for data, visibility,
access control, transactions, and recovery.

Start with the [Quickstart](quickstart.md), choose an
[installation method](user_guide/installation.md), and run the packaged
[playground](user_guide/playground.md) to explore the APIs. Before adopting
experimental features in production, review
[known issues](known_issues.md), [known limitations](user_guide/limitations.md),
and the [0.2.0 release notes](release_notes.md).

## User Documentation

- [Installation and verification](user_guide/installation.md)
- [Configuration](user_guide/configuration.md)
- [Collections](user_guide/collections.md)
- [Vector search](user_guide/vector_search.md)
- [HNSW indexes](user_guide/indexes.md)
- [Metadata filters](user_guide/filters.md)
- [Hybrid retrieval](user_guide/hybrid_retrieval.md)
- [pgContext vs. pgvector vs. Qdrant](pgcontext-vs-pgvector-vs-qdrant.md)
- [pgContext vs. pgvector benchmark](benchmarks/pgvector.md)
- [Owned late-interaction storage and write amplification](benchmarks/late_interaction_owned.md)
- [Operations and security](user_guide/operations.md)
- [Troubleshooting](user_guide/troubleshooting.md)
- [SQL API](user_guide/api_reference.md)

## Project Documentation

- [Release notes](release_notes.md)
- [Roadmap](roadmap.md)
- [Known issues](known_issues.md)
- [Contributor guide](contributor_guide/README.md)
- [Release tooling](../release/README.md)
