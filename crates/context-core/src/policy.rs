//! Default policy values shared by pgContext crates.
//!
//! SQL-visible settings may override these defaults in later milestones, but
//! the built-in defaults live here to avoid drift across crates.

/// PostgreSQL major version used for primary development and release gates.
pub const PRIMARY_POSTGRES_VERSION: u16 = 17;

/// Lowest PostgreSQL major version with first-class release support.
pub const MIN_SUPPORTED_POSTGRES_VERSION: u16 = 17;

/// Highest PostgreSQL major version with first-class release support.
pub const MAX_SUPPORTED_POSTGRES_VERSION: u16 = 18;

/// Default maximum number of nearest-neighbor results returned by one exact search.
pub const MAX_SEARCH_LIMIT: usize = 10_000;

/// Maximum exact or candidate point IDs accepted by one SQL recall check.
pub const MAX_RECALL_CHECK_POINT_IDS: usize = 10_000;

/// Maximum application query stages represented or executed in one request.
pub const MAX_QUERY_STAGES: usize = 256;

/// Maximum adaptive candidate expansions allowed in one query execution.
pub const MAX_QUERY_EXPANSIONS: usize = 64;

/// Default maximum distinct point IDs accepted by one HNSW candidate mask.
///
/// This is a caller-suppliable default, not a hard ceiling: the AM masked
/// scan path (`pgcontext.hnsw_mask_candidate_limit`) can raise it. It used
/// to alias `MAX_RECALL_CHECK_POINT_IDS`, an unrelated recall-check budget;
/// the two were decoupled so growing this value does not silently move an
/// unrelated policy.
pub const DEFAULT_HNSW_CANDIDATE_MASK_POINTS: usize = 10_000;

/// Highest value `pgcontext.hnsw_mask_candidate_limit` may be set to.
pub const MAX_HNSW_CANDIDATE_MASK_POINTS: usize = 5_000_000;

/// Default candidate budget for filtered or iterative HNSW search.
pub const DEFAULT_HNSW_CANDIDATE_BUDGET: usize = DEFAULT_HNSW_EF_SEARCH;

/// Maximum SQL-configurable HNSW candidate budget.
pub const MAX_HNSW_CANDIDATE_BUDGET: usize = MAX_RECALL_CHECK_POINT_IDS;

/// Default candidate batch ceiling for iterative HNSW expansion.
pub const DEFAULT_HNSW_ITERATIVE_EXPANSION_LIMIT: usize = MAX_RECALL_CHECK_POINT_IDS;

/// Maximum SQL-configurable iterative HNSW expansion ceiling.
pub const MAX_HNSW_ITERATIVE_EXPANSION_LIMIT: usize = MAX_RECALL_CHECK_POINT_IDS;

/// Default minimum recall expected for approximate HNSW serving health.
pub const DEFAULT_HNSW_RECALL_THRESHOLD: f64 = 0.95;

/// Default HNSW bulk-build worker count (single-threaded, deterministic).
pub const DEFAULT_HNSW_BUILD_PARALLEL_WORKERS: usize = 1;

/// Highest value `pgcontext.hnsw_build_parallel_workers` may be set to.
pub const MAX_HNSW_BUILD_PARALLEL_WORKERS: usize = 128;

/// Default maximum retained HNSW neighbors per node.
pub const DEFAULT_HNSW_M: usize = 16;

/// Minimum HNSW neighbor count that can preserve reciprocal connectivity.
pub const MIN_HNSW_M: usize = 2;

/// Default HNSW construction candidate budget.
pub const DEFAULT_HNSW_EF_CONSTRUCTION: usize = 64;

/// Default HNSW search candidate budget.
pub const DEFAULT_HNSW_EF_SEARCH: usize = 32;

/// Maximum SQL-configurable HNSW neighbor count.
pub const MAX_HNSW_M: usize = 128;

/// Maximum SQL-configurable HNSW construction candidate budget.
pub const MAX_HNSW_EF_CONSTRUCTION: usize = 4096;

/// Maximum SQL-configurable HNSW search candidate budget.
pub const MAX_HNSW_EF_SEARCH: usize = 4096;

/// Maximum byte length for collection names stored in pgContext catalogs.
pub const MAX_COLLECTION_NAME_BYTES: usize = 63;

/// Maximum byte length for SQL identifiers stored in pgContext catalogs.
pub const MAX_SQL_IDENTIFIER_BYTES: usize = 63;

/// Maximum vector dimensions or bit length accepted by core vector types.
pub const MAX_VECTOR_DIMENSIONS: usize = 16_000;

/// Maximum byte length for a source row key stored in point mappings.
pub const MAX_SOURCE_KEY_BYTES: usize = 1024;

/// Maximum nested boolean-filter depth accepted before SQL planning.
pub const MAX_FILTER_DEPTH: usize = 16;

/// Maximum number of filter condition nodes accepted before SQL planning.
pub const MAX_FILTER_NODES: usize = 256;

/// Maximum byte length for one filter field key.
pub const MAX_FILTER_KEY_BYTES: usize = 512;

/// Maximum number of dotted path segments in one filter field key.
pub const MAX_FILTER_PATH_DEPTH: usize = 16;
