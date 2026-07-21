# pgContext vs. pgvector vs. Qdrant

This document gives a capability-by-capability comparison of pgContext
0.1.0 against pgvector 0.8.5 and Qdrant 1.18.2.

All three tools power vector search, but they operate at different layers
of the stack:

- **pgvector** provides mature, low-level vector types and nearest-neighbor
  indexing directly in Postgres, well suited to raw SQL composition.
- **Qdrant** is a dedicated, distributed vector database. It takes full
  ownership of collections, points, and payloads behind REST and gRPC APIs.
- **pgContext** sits in between as a PostgreSQL-native retrieval layer: it
  provides Qdrant-like collections, dynamic filters, filtered ANN, hybrid
  search, facets, grouping, and recommendations, but executes entirely
  against existing, authoritative PostgreSQL tables.

This comparison distinguishes missing features from behaviors that
PostgreSQL SQL can already assemble, and labels experimental paths
explicitly.

The comparison baselines are:

- pgvector's official [v0.8.5 documentation](https://github.com/pgvector/pgvector/tree/v0.8.5);
- Qdrant's official [v1.18.2 release](https://github.com/qdrant/qdrant/releases/tag/v1.18.2),
  [overview](https://qdrant.tech/documentation/overview/),
  [search documentation](https://qdrant.tech/documentation/search/), and
  [data-management documentation](https://qdrant.tech/documentation/manage-data/);
- pgContext's local [SQL API contract](user_guide/api_reference.md),
  [parity matrix](user_guide/parity_matrix.md), and
  [known limitations](user_guide/limitations.md).

For measured PostgreSQL results, see the reproducible
[pgContext vs. pgvector vs. Qdrant benchmark](benchmarks/pgvector.md). In the
three-trial matched-recall run, pgContext HNSW reaches 0.157 ms
p50 at 0.9900 recall@10, compared with pgvector's 0.179 ms at 0.9902 recall
and Qdrant's local-gRPC 0.690 ms at 1.0000 recall, and pgContext's
latency/recall curve Pareto-dominates pgvector's on both the 5k and 100k
sweeps. Build time (pgContext is slower), multi-client throughput and memory
(pgvector currently leads at 32 clients), filtered quality/latency, scale,
and operational maturity remain separate dimensions and are reported rather
than collapsed into one winner.

## Status legend

| Status | Meaning |
|---|---|
| **Stable** | Included in pgContext's first stable SQL compatibility surface. |
| **Experimental** | Implemented and SQL-visible, but outside pgContext's stable compatibility promise. |
| **Partial** | Some equivalent behavior exists, with material gaps listed in the caveat. |
| **Planned** | Not implemented as a serving path. |
| **SQL/application** | Not a dedicated product API, but PostgreSQL SQL or application code can build the behavior. |
| **Native** | A documented, product-supported pgvector or Qdrant capability. |

## Executive comparison

| Area | pgContext 0.1.0 | pgvector 0.8.5 | Qdrant 1.18.2 | Practical difference |
|---|---|---|---|---|
| Primary abstraction | Retrieval system over registered PostgreSQL tables | Vector types, operators, and ANN indexes | Dedicated vector database of collections and points | pgContext and Qdrant expose retrieval APIs; pgvector exposes lower-level SQL primitives. |
| Authoritative data | Existing application tables | PostgreSQL vector columns | Qdrant points and payloads | Qdrant normally requires copying/synchronizing application data; pgContext and pgvector keep it in PostgreSQL. |
| Exact dense search | **Stable** | **Native** | [**Native**](https://qdrant.tech/documentation/search/search/) | All three support exact search. |
| HNSW | **Implemented; PostgreSQL 17 benchmark-qualified** | **Native** | [**Native**](https://qdrant.tech/documentation/manage-data/indexing/#vector-index) | pgContext wins the current small-corpus matched-recall latency run; pgvector and Qdrant retain broader production history. |
| IVFFlat | Not implemented | **Native** | Not implemented | IVFFlat is a pgvector advantage. |
| Dynamic metadata filters | **Stable** registered JSON filter API | **Native SQL** `WHERE` | [**Native** payload filters](https://qdrant.tech/documentation/search/filtering/) | pgContext and Qdrant provide structured filter grammars; pgvector allows arbitrary SQL. |
| Filter-aware ANN | **Implemented** adaptive exact/masked traversal with ACORN-like sparse expansion | Post-filter plus iterative scan | [**Native** filterable HNSW and ACORN](https://qdrant.tech/documentation/manage-data/indexing/#filterable-hnsw-index) | All three work dynamically; their planning and graph strategies are not identical. |
| Collections and points | **Stable** metadata/mappings over source tables | **SQL/application** | [**Native**](https://qdrant.tech/documentation/manage-data/collections/) | pgContext references PostgreSQL rows; Qdrant owns its points. |
| Hybrid retrieval | **Stable** dense + PostgreSQL FTS RRF | **SQL/application** | [**Native** dense+sparse RRF/DBSF and multistage queries](https://qdrant.tech/documentation/search/hybrid-queries/) | Qdrant has the broadest built-in query pipeline; pgContext integrates PostgreSQL FTS. |
| Facets, scroll, grouping | **Stable** | **SQL/application** | [**Native**](https://qdrant.tech/documentation/search/) | pgContext and Qdrant supply dedicated APIs. |
| Recommendation/discovery | **Stable** exact paths | **SQL/application** | [**Native**](https://qdrant.tech/documentation/search/explore/) | pgContext and Qdrant supply dedicated APIs. |
| Horizontal scaling | PostgreSQL deployment architecture | PostgreSQL deployment architecture | [Native sharding and replication](https://qdrant.tech/documentation/distributed_deployment/) | Qdrant is designed to scale independently as a vector service. |
| Transactions with relational data | Same PostgreSQL transaction boundary | Same PostgreSQL transaction boundary | Separate service and consistency boundary | This is a core pgContext/pgvector advantage for transactional applications. |
| Ecosystem maturity | Initial V1 distribution | Broad PostgreSQL 13+ packaging | Mature OSS, cloud, clients, and distributed deployment | pgvector and Qdrant are lower-risk production defaults today. |

## Architecture and data ownership

| Capability | pgContext | pgvector | Qdrant | Parity and caveat |
|---|---|---|---|---|
| Runs inside PostgreSQL | Yes | Yes | No, separate REST/gRPC service | Qdrant introduces another deployment, network, authentication, and recovery boundary. |
| Reuses existing application rows | **Stable** registered tables | **Native** vector columns | Requires point ingestion into Qdrant | pgContext catalogs reference source tables without copying rows. |
| Relational joins and constraints | **Native PostgreSQL** | **Native PostgreSQL** | Application-side or limited lookup APIs | Qdrant is not a relational database. |
| Foreign keys/triggers/views | **Native PostgreSQL** | **Native PostgreSQL** | Not equivalent | Qdrant payloads do not replace relational constraints. |
| Point/payload ownership | PostgreSQL rows authoritative; pgContext owns mappings/configuration | PostgreSQL rows authoritative | [Qdrant owns points, vectors, and payloads](https://qdrant.tech/documentation/concepts/points/) | Synchronization is required when Qdrant mirrors another primary store. |
| Independent vector scaling | Scales with PostgreSQL topology | Scales with PostgreSQL topology | [Native shards, replicas, and nodes](https://qdrant.tech/documentation/distributed_deployment/) | Qdrant can isolate vector workload scaling from the application database. |

## Vector types and metrics

| Capability | pgContext | pgvector | Qdrant | Parity and caveat |
|---|---|---|---|---|
| Dense vectors | **Stable** `vector(n)`, 1–16,000 dimensions | **Native** `vector`; ANN indexing up to documented type limits | [**Native** dense vectors](https://qdrant.tech/documentation/manage-data/vectors/#dense-vectors) | pgContext and pgvector expose SQL types; Qdrant exposes API collection schemas. |
| Dense storage datatypes | Stable float32 vector; experimental half type | Float32 `vector`, `halfvec`, and expression-based alternatives | [Float32, Float16, and Uint8](https://qdrant.tech/documentation/manage-data/vectors/#datatypes) | Qdrant supports storage datatype selection per vector configuration. |
| L2/Euclidean | **Stable** | **Native** | [**Native** `Euclid`](https://qdrant.tech/documentation/search/#metrics) | Qdrant returns similarity/distance semantics through its API; exact score values may differ in convention. |
| Inner/dot product | **Stable** | **Native** | [**Native** `Dot`](https://qdrant.tech/documentation/search/#metrics) | pgContext/pgvector use negative inner product for ascending index ordering. |
| Cosine | **Stable** | **Native** | [**Native** `Cosine`](https://qdrant.tech/documentation/search/#metrics) | Qdrant normalizes cosine vectors at ingestion; compare score conventions during migration. |
| L1/Manhattan | **Stable** | **Native** | [**Native** `Manhattan`](https://qdrant.tech/documentation/search/#metrics) | Functional overlap. |
| Hamming/Jaccard binary metrics | **Experimental** exact; partial ANN | **Native** | No general binary-vector Hamming/Jaccard type; binary quantization is separate | pgvector is strongest for explicit binary-vector similarity. |
| SQL distance operators | **Stable** `<->`, `<#>`, `<=>`, `<+>` | **Native** | Not applicable | Qdrant uses request schemas rather than SQL operators. |
| Typmod dimension enforcement | **Stable** `vector(n)` | **Native** `vector(n)` | Collection vector schema enforces dimensions | Same goal through different interfaces. |
| Numeric-array casts | **Stable** | **Native** | Client serialization | SQL casting is not applicable to Qdrant. |
| Vector sum/average aggregates | **Stable** | **Native** | Application/query pipeline, not SQL aggregates | PostgreSQL is more flexible for arbitrary vector analytics. |
| Half-precision vectors | **Experimental** | **Native** `halfvec` | [**Native** Float16 storage](https://qdrant.tech/documentation/manage-data/vectors/#datatypes) | Interfaces and index implementations differ. |
| Sparse vectors | **Experimental** exact and partial ANN | **Native** `sparsevec` and HNSW | [**Native** sparse vectors and sparse index](https://qdrant.tech/documentation/manage-data/vectors/#sparse-vectors) | Qdrant and pgvector are substantially more mature than pgContext for sparse serving. |
| Named vectors | **Stable** registered dense vectors | Multiple columns through SQL | [**Native** multiple named vectors](https://qdrant.tech/documentation/manage-data/collections/#collection-with-multiple-vectors) | Qdrant and pgContext expose names at the retrieval API. |
| Multivectors/MaxSim | **Experimental** | **SQL/application** | [**Native** multivectors with MaxSim](https://qdrant.tech/documentation/manage-data/vectors/#multivectors) | Qdrant has the mature first-class late-interaction representation. |
| Arbitrary source columns beside vectors | **Native PostgreSQL** | **Native PostgreSQL** | JSON payload plus point vectors | PostgreSQL supports richer types and relational modeling. |

pgContext and pgvector types have different PostgreSQL OIDs and are not
interchangeable even when both display as `vector`.

## Exact and approximate indexing

| Capability | pgContext | pgvector | Qdrant | Parity and caveat |
|---|---|---|---|---|
| Exact scan | **Stable** | **Native** | [**Native** exact query option](https://qdrant.tech/documentation/search/search/) | All three support a correctness-oriented exact path. |
| HNSW dense four-metric serving | **Implemented; PostgreSQL 17 benchmark-qualified** | **Native** | [**Native**](https://qdrant.tech/documentation/manage-data/indexing/#vector-index) | Functional parity for dense L2, cosine, inner product, and L1; lifecycle and multi-version qualification differ. |
| HNSW build/search tuning | **Implemented** GUCs and budgets | **Native** `m`, `ef_construction`, `ef_search` | [**Native** `m`, `ef_construct`, per-query `ef`](https://qdrant.tech/documentation/manage-data/indexing/#vector-index) | Similar concepts, different defaults and planner behavior. |
| Disable HNSW/use full scan | Omit HNSW index | Omit ANN index | Set `m=0` or use optimizer/exact-query controls | All can avoid ANN when exact scanning is appropriate. |
| IVFFlat | Not implemented | **Native** | Not implemented | Unique pgvector advantage among these products. |
| Iterative ANN expansion | One-pass masked traversal; selective masks cross over to exact | **Native** strict/relaxed iterative scans | Qdrant planner/filterable graph/ACORN instead | These mechanisms should not be labeled 1:1 parity. |
| Parallel HNSW build | No mature parity | **Native** PostgreSQL parallel workers | Background segment optimization | Operational models differ; Qdrant rebuilds indexes as segments optimize. |
| Standard index build progress | Limited | **Native** `pg_stat_progress_create_index` | Collection/optimizer status and telemetry | pgvector has the clearest PostgreSQL-native progress integration. |
| Partial vector indexes | Not a documented pgContext retrieval contract | **Native SQL** partial indexes | Payload-index/filterable-graph model | pgvector can maintain one partial HNSW per predicate; Qdrant takes a different approach. |
| Expression/subvector indexes | **Partial** | **Native** | Named vectors, multivectors, and query stages instead | pgvector is most flexible for SQL expression indexing. |
| Exact reranking | Built into filtered source recheck; helpers for advanced paths | **SQL/application** | [**Native** multistage/rescore paths](https://qdrant.tech/documentation/search/hybrid-queries/) | Qdrant's Query API has the broadest built-in multistage execution. |
| Recall evaluation | **Stable** typed `recall_check` | **SQL/application** | Exact-query comparison and evaluation tooling | pgContext uniquely ships a small typed recall helper; this is not a benchmark claim. |

## Metadata filtering

### Indexes are performance tools, not universal correctness requirements

The term “index” refers to three distinct operations:

1. **Declaring/registering a field** makes it available to a structured filter
   API. pgContext requires registration. Qdrant accepts payload fields without
   a payload index unless strict mode blocks unindexed filtering.
2. **Indexing metadata** accelerates matching and cardinality estimation.
   pgContext uses optional PostgreSQL B-tree/GIN indexes. Qdrant uses optional
   payload indexes, although Qdrant Cloud strict-mode defaults can require them.
3. **Indexing vectors** accelerates approximate similarity search. pgContext,
   pgvector, and Qdrant can all use exact scanning without HNSW.

Consequently:

- pgContext exact filtered search needs neither a metadata index nor HNSW;
- pgContext filtered ANN needs HNSW, but a metadata index remains optional;
- pgvector `WHERE` filters need no metadata index, though ordinary indexes can
  improve plans;
- Qdrant OSS can evaluate unindexed payload filters when strict mode permits
  them, but [recommends payload indexes](https://qdrant.tech/documentation/search/filtering/)
  for performance;
- Qdrant payload indexes created before vector indexing can add filter-aware
  HNSW edges. Creating a payload index later may require HNSW reconstruction to
  gain that optimization.

### Filter capability matrix

| Capability | pgContext | pgvector | Qdrant | Parity and caveat |
|---|---|---|---|---|
| Per-query dynamic filters | **Stable** | **Native SQL** | [**Native**](https://qdrant.tech/documentation/search/filtering/) | None requires a separate vector index for every filter value or combination. |
| Field declaration required | Yes for pgContext JSON API | No | No for basic payload use; schema/index required for indexed or strict-mode paths | pgContext registration does not build an index. |
| Ordinary scalar equality | **Stable** | **Native SQL** | **Native** match value | Functional overlap. |
| Match any/except | **Stable** | **Native SQL** | **Native** match any/except | Functional overlap. |
| Numeric ranges | **Stable** | **Native SQL** | **Native** range | Functional overlap. |
| Datetime ranges | PostgreSQL typed columns/registered comparison | **Native SQL** | [**Native** RFC 3339 datetime range](https://qdrant.tech/documentation/search/filtering/#datetime-range) | Qdrant has a dedicated condition; PostgreSQL has richer date/time SQL. |
| Nested JSON/object filters | **Stable** registered JSONB paths | **Native SQL/JSONB** | [**Native** nested payload filters](https://qdrant.tech/documentation/search/filtering/) | Qdrant can preserve nested-array element relationships; test semantic differences. |
| AND/OR/NOT | **Stable** `must`/`should`/`must_not` | **Native SQL** | **Native** `must`/`should`/`must_not` | pgContext intentionally resembles Qdrant's grammar. |
| Null/empty checks | **Stable** | **Native SQL** | **Native** is-null/is-empty conditions | Functional overlap with different missing/null semantics. |
| Full-text condition | Hybrid FTS branch; not generic JSON-filter text search | **Native PostgreSQL FTS** | Native text payload index/filter | PostgreSQL FTS and Qdrant text filtering are not identical ranking systems. |
| Geospatial conditions | PostgreSQL/PostGIS SQL outside current JSON grammar | **Native SQL/PostGIS** | **Native** radius, bounding-box, and polygon conditions | Qdrant has first-class geo payload filters. |
| Payload value count | Not in current JSON grammar | **Native SQL/JSONB** | **Native** values-count condition | Qdrant is broader at the structured-filter API layer. |
| Point-ID condition | Candidate point overloads | **Native SQL** primary-key predicate | **Native** has-id condition | Functional overlap. |
| Arbitrary joins/subqueries | Not through pgContext JSON grammar; available in external SQL | **Native SQL** | Not general relational joins | pgvector/PostgreSQL is the most expressive. |
| Metadata index required for filtering | No | No | No when strict mode permits; recommended for performance | Qdrant Cloud blocks many unindexed paths by default through strict mode. |
| Facet field index required | No separate requirement beyond registration; ordinary index recommended | SQL planner choice | [Yes, compatible payload index](https://qdrant.tech/documentation/concepts/payload/#facet-counts) | Important Qdrant caveat. |
| Shared grammar for search/count/facet | **Stable** | **SQL/application** | **Native** | pgContext and Qdrant offer cohesive retrieval APIs. |
| Dynamic-query safety | Typed AST and bound SPI parameters | Parameterized SQL/application responsibility | Structured REST/gRPC request schema | All can be safe when used as designed. |
| Row-level access policy | PostgreSQL RLS/ACL source rechecks | PostgreSQL RLS/ACL | API keys/JWT collection and payload-value access controls | Qdrant security is not PostgreSQL row-level security. |

### Filtered ANN execution

The three approximate strategies differ:

- **pgvector:** the executor applies `WHERE` filtering after HNSW/IVFFlat
  candidates are produced. Iterative scans can request more candidates until
  enough survive, subject to scan/memory limits. Partial indexes and table
  partitioning are additional strategies.
- **pgContext:** the registered path counts a bounded candidate
  set once, uses exact scoring below the selectivity crossover, or passes one
  reusable mask into persisted HNSW above it. Excluded nodes remain connectors;
  sparse masks can expand through second-hop neighbors before point activity,
  MVCC, ACL/RLS, predicate, and exact source-distance rechecks.
- **Qdrant:** the query planner uses payload cardinality estimates to choose a
  full scan or graph path. Payload indexes can extend HNSW with filter-aware
  edges so filtering occurs during traversal. [ACORN](https://qdrant.tech/documentation/manage-data/indexing/#the-acorn-search-algorithm)
  can explore second-hop neighbors for difficult combined filters.

Qdrant remains the distributed reference implementation for dedicated
filter-aware vector search. pgContext's path keeps PostgreSQL rows
authoritative and, in the current benchmark, delivers lower raw filtered
latency than Qdrant at 0.9935 versus 1.0000 recall; its explicit masked path
matches 1.0000 recall with higher latency on this small 10%-selectivity case.

## Retrieval and collection APIs

Entries marked **SQL/application** do not mean pgvector cannot produce the
result; they mean it does not ship a dedicated API for that workflow.

| Capability | pgContext | pgvector | Qdrant | Parity and caveat |
|---|---|---|---|---|
| Collections | **Stable** metadata over source tables | **SQL/application** tables/schemas | [**Native** owned collections](https://qdrant.tech/documentation/manage-data/collections/) | pgContext references rows; Qdrant stores points. |
| Point IDs | **Stable** logical IDs mapped to source keys | **SQL/application** primary keys | [**Native** uint64 or UUID](https://qdrant.tech/documentation/concepts/points/#point-ids) | pgContext mapping supports external relational keys. |
| Upsert/delete points | **Stable** mapping helpers plus source SQL | **Native SQL** | [**Native** point API](https://qdrant.tech/documentation/concepts/points/) | Qdrant operations mutate its primary point store. |
| Bulk upload/backfill | **Stable** bounded mapping/backfill helpers | PostgreSQL `COPY`/SQL | **Native** batching/parallel upload APIs | All support bulk workflows with different ownership. |
| Collection aliases | **Stable** | **SQL/application** views/routing | [**Native**, atomic alias switch](https://qdrant.tech/documentation/manage-data/collections/#collection-aliases) | pgContext and Qdrant both support model cutovers. |
| Named dense vectors | **Stable** | Multiple SQL columns | **Native** | pgContext and Qdrant expose retrieval-level names. |
| Named sparse vectors | **Experimental** exact | **Native** type/index through columns | **Native** sparse-vector configuration/index | Qdrant is strongest at the integrated API level. |
| Per-vector HNSW/quantization configuration | **Experimental** metadata; not all consumed | Index/operator configuration | **Native** per named vector | Qdrant configuration is production-serving behavior. |
| Scroll | **Stable** | **SQL/application** keyset pagination | [**Native** filtered scroll](https://qdrant.tech/documentation/concepts/points/#scroll-points) | Qdrant can also order by indexed payload keys. |
| Count | **Stable** exact active mappings | **Native SQL** | [**Native** approximate or exact](https://qdrant.tech/documentation/concepts/points/#counting-points) | State exactness explicitly when comparing results. |
| Facets | **Stable** | **Native SQL** `GROUP BY` | [**Native** approximate or exact](https://qdrant.tech/documentation/concepts/payload/#facet-counts) | Qdrant facets require a compatible payload index. |
| Grouped search | **Stable**, exact | **SQL/application** | [**Native** group query](https://qdrant.tech/documentation/search/search/#grouping-api) | Qdrant grouping supports keyword/integer payload fields and no group pagination. |
| Payload set/delete/clear | **Stable** registered-field mutation | **Native SQL** | [**Native** payload API](https://qdrant.tech/documentation/concepts/payload/#update-payload) | pgContext intentionally limits mutations to registered source fields. |
| Recommendation | **Stable**, exact | **SQL/application** | [**Native** positive/negative IDs or vectors](https://qdrant.tech/documentation/search/explore/#recommendation-api) | Qdrant supports multiple recommendation strategies. |
| Discovery/context search | **Stable**, exact | **SQL/application** | [**Native** discovery search](https://qdrant.tech/documentation/search/explore/) | The scoring definitions are not guaranteed to be identical. |
| Random sampling | **SQL/application** | **Native SQL** | **Native** query type | No dedicated pgContext API today. |
| Candidate recheck/multistage query | **Stable** candidate overload; advanced paths experimental | **SQL/application** CTE/subquery | [**Native** nested prefetch/query](https://qdrant.tech/documentation/search/hybrid-queries/) | Qdrant has the most general built-in execution graph. |
| Dense + full-text hybrid | **Stable** PostgreSQL FTS + RRF | **SQL/application** | Dense+sparse/text-index patterns, not PostgreSQL FTS | pgContext uniquely fuses with the application's PostgreSQL text data directly. |
| Dense + sparse hybrid | **Experimental** exact RRF | **SQL/application** | [**Native** RRF and DBSF](https://qdrant.tech/documentation/search/hybrid-queries/#hybrid-search) | Qdrant is substantially ahead. |
| Weighted/custom ranking formula | Constructors stable; full execution partial | **SQL/application** | [**Native** formula query and decay functions](https://qdrant.tech/documentation/search/hybrid-queries/#custom-scoring-with-a-formula-query) | Qdrant has the mature integrated surface. |
| Late interaction/MaxSim | **Experimental** exact plus user-managed token candidates | **SQL/application** | [**Native** multivector MaxSim](https://qdrant.tech/documentation/manage-data/vectors/#multivectors) | Qdrant is substantially ahead. |
| Cross-collection lookup | PostgreSQL joins | **Native SQL** joins | Group lookup by matching point ID | PostgreSQL provides general joins; Qdrant's lookup is deliberately narrow. |
| External cross-encoder | Application | Application | Application | None runs an arbitrary external model by itself. |

## Quantization

| Capability | pgContext | pgvector | Qdrant | Parity and caveat |
|---|---|---|---|---|
| Binary quantization helper | **Experimental** | **Native** function/expression index | [**Native** serving](https://qdrant.tech/documentation/quantization/) | pgContext does not yet serve quantized HNSW candidates. |
| Scalar/SQ8 quantization | **Experimental** encode/reconstruct helper | No dedicated serving type | [**Native** scalar quantization](https://qdrant.tech/documentation/quantization/) | Qdrant is production-serving; pgContext is a helper surface only. |
| Product quantization | **Experimental** encode/reconstruct helper | No dedicated core serving feature | [**Native** product quantization](https://qdrant.tech/documentation/quantization/) | Qdrant is production-serving. |
| TurboQuant | Not implemented | Not implemented | [**Native**](https://qdrant.tech/documentation/quantization/) | Qdrant-only in this comparison. |
| Quantized index traversal | **Planned** | Binary expression-index pattern | **Native** | Do not infer pgContext serving support from quantization metadata. |
| Exact/full-precision rescore | **Experimental** helper; exact source rechecks on implemented paths | **SQL/application** | **Native** query-time rescore controls | Qdrant has the broadest integrated quantized serving path. |

## Security, durability, and operations

| Capability | pgContext | pgvector | Qdrant | Parity and caveat |
|---|---|---|---|---|
| Authentication | PostgreSQL roles/authentication | PostgreSQL roles/authentication | [API keys, read-only keys, JWT RBAC](https://qdrant.tech/documentation/security/) | Qdrant self-hosted is unsecured by default and must be configured. |
| Encryption in transit | PostgreSQL TLS | PostgreSQL TLS | [TLS configuration](https://qdrant.tech/documentation/security/#tls) | Operational configuration differs. |
| Row-level security | PostgreSQL RLS with source rechecks | PostgreSQL RLS | Collection/payload-scoped JWT controls, not relational RLS | PostgreSQL is stronger for policies tied to relational data and session roles. |
| Relational transaction atomicity | Same transaction as application tables | Same transaction as application tables | Separate point-operation boundary | Qdrant does not provide one atomic transaction with PostgreSQL application writes. |
| Local durability | PostgreSQL WAL | PostgreSQL WAL | [Qdrant WAL and segment versioning](https://qdrant.tech/documentation/manage-data/storage/#versioning) | All have durable local update mechanisms. |
| Distributed consistency | PostgreSQL replication/topology | PostgreSQL replication/topology | Configurable ordering/consistency; Raft for cluster metadata | Qdrant docs explicitly do not claim atomic distributed point updates. |
| Backup/restore | PostgreSQL backup, WAL, PITR | PostgreSQL backup, WAL, PITR | [Collection/shard snapshots](https://qdrant.tech/documentation/operations/snapshots/) | Qdrant snapshots require compatible version procedures and cover Qdrant data, not the application DB. |
| Replication/sharding | PostgreSQL deployment choice | PostgreSQL deployment choice | [Native distributed deployment](https://qdrant.tech/documentation/distributed_deployment/) | Qdrant can independently shard and replicate vector data. |
| Online alias cutover | **Stable** | **SQL/application** | **Native**, atomic alias actions | Useful for embedding migrations in pgContext and Qdrant. |
| Index lifecycle maturity | Page-native storage engine; PostgreSQL 17 qualified | Mature | Mature segment optimizer/HNSW | pgContext has the newest lifecycle, with PostgreSQL WAL/backup authority and rebuild-on-format-change rules. |
| On-disk index compatibility | Early format; rebuilds may be required | Mature extension upgrade path | Product upgrade/snapshot compatibility policy | Validate all upgrades; Qdrant snapshots are limited by documented version compatibility. |
| Recall diagnostics | **Stable** typed helper | **SQL/application** | Exact comparison/evaluation tooling | Not a statement of equal observability. |
| Index advisor | **Stable** | DBA/query-planner workflow | Optimizer and collection configuration guidance | Different operational models. |
| Metrics/telemetry | **Stable** SQL collection/cohort counters | PostgreSQL statistics | [Prometheus/OpenMetrics and telemetry endpoints](https://qdrant.tech/documentation/ops-monitoring/monitoring/) | Qdrant provides service/cluster metrics; pgContext keeps retrieval telemetry in PostgreSQL. |
| Horizontal autoscaling/rebalancing | PostgreSQL platform-dependent | PostgreSQL platform-dependent | Cloud supports managed balancing/resharding; self-hosted requires more manual work | Do not conflate Qdrant Cloud features with OSS defaults. |
| Strict resource/query controls | **Stable/partial** collection limits | PostgreSQL roles/GUCs/resource controls | [Native strict mode](https://qdrant.tech/documentation/overview/#safety) | Qdrant strict mode is broader; some pgContext stored limits are not yet consumed by every path. |
| Packaging/deployment | Initial V1 packages | Broad PostgreSQL package ecosystem | Docker, binary, Kubernetes/Helm, cloud, client SDKs | pgvector and Qdrant are more mature operationally. |

## Compatibility and migration

### From pgvector to pgContext

pgContext is not a drop-in replacement for pgvector:

- the extensions define different PostgreSQL types and OIDs;
- existing pgvector columns and indexes cannot be assumed to work with
  pgContext registration or `pgcontext_hnsw`;
- pgContext lacks full pgvector helper, expression/subvector, iterative-scan,
  parallel-build, progress-reporting, non-dense ANN, and IVFFlat parity;
- coexistence and in-place conversion have not graduated into a stable contract.

See [Migrating from pgvector](user_guide/pgvector_migration.md).

### From Qdrant to pgContext

The filter JSON and collection vocabulary are intentionally familiar, but the
systems are not API-compatible:

- Qdrant points own vectors and JSON payload; pgContext collections reference
  PostgreSQL tables and registered fields;
- Qdrant point IDs must be mapped to PostgreSQL source keys and pgContext point
  mappings;
- Qdrant geo, text, values-count, multivector, sparse ANN, formula-query,
  quantization, sharding, and distributed APIs do not all have pgContext parity;
- Qdrant similarity scores and pgContext distance scores may use different
  direction or normalization conventions;
- filters must be translated and their missing/null/array semantics tested;
- application data synchronization can be removed only after PostgreSQL has
  become the verified authoritative store for every required field.

A safe migration exports into a separate test database, recreates relational
constraints and indexes, registers collection fields, then validates exact
results, ANN recall, filters, permissions, latency, backup/recovery, and cutover
behavior.

## Which Should You Choose?

Choose **pgvector** when you want mature vector storage and ANN indexing composed directly through SQL. It’s perfect if you need IVFFlat, mature half/sparse/bit indexes, expression/subvector indexes, or if you prefer minimal abstraction above SQL.

Choose **Qdrant** if vector retrieval is a separate microservice and you need a mature distributed vector database with built-in sharding, replication, and rich payload filters—and you're completely comfortable managing a second data store and keeping it synchronized.

Choose **pgContext** when you want **Qdrant-like retrieval workflows, but want PostgreSQL to remain the absolute source of truth.** If you love the idea of registered dynamic filters, filter-first ANN evaluation, collections over existing tables, facets, scrolling, and hybrid PostgreSQL FTS fusion—all protected by your existing relational transactions and Row-Level Security—pgContext is a strong fit.

## The Bottom Line

- **pgvector** is the reigning champion of mature, flexible PostgreSQL vector types and index extensions.
- **Qdrant** is a powerhouse distributed vector database with filter-aware indexing and a broad native query API.
- **pgContext** places a high-level, AI-ready retrieval contract directly over your authoritative PostgreSQL rows: structured filters, hybrid retrieval, and operational controls, with no second data store to run or keep in sync.

pgContext's dense HNSW and filtered ANN paths are implemented and measured.
Non-dense, quantized, and late-interaction serving are on the roadmap, so
capability claims stay specific to each row above.
