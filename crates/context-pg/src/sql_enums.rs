use pgrx::prelude::*;

/// Backend-local resumable build status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PostgresEnum)]
pub enum BuildJobStatus {
    /// Build metadata exists before execution starts.
    Planned,
    /// The backend recorded in the job is actively processing the build.
    Running,
    /// Cancellation was requested and the owning backend should stop safely.
    CancelRequested,
    /// The build stopped after a cancellation request.
    Cancelled,
    /// The build completed successfully.
    Completed,
    /// The build stopped with a reported failure.
    Failed,
    /// The recorded backend is gone before completion.
    Abandoned,
}

/// Embedding migration lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PostgresEnum)]
pub enum EmbeddingMigrationStatus {
    /// Migration has been registered but no backfill is running.
    Planned,
    /// Backfill is currently in progress.
    Running,
    /// Backfill has processed all planned points.
    Completed,
    /// Backfill failed and needs operator attention.
    Failed,
}

/// Query serving lifecycle state recorded with telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PostgresEnum)]
pub enum QueryLifecycleState {
    /// Older or coarse samples did not declare a lifecycle state explicitly.
    Unspecified,
    /// The query used exact table-backed scoring.
    Exact,
    /// The query used an indexed serving path.
    Indexed,
    /// The query fell back from an optimized path to exact or conservative serving.
    Fallback,
    /// The selected index path was not ready.
    IndexNotReady,
    /// The selected index or artifact was corrupt.
    IndexCorrupt,
    /// A rebuildable artifact was missing.
    ArtifactMissing,
}

/// Aggregated query cohort state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PostgresEnum)]
pub enum QueryCohortStatus {
    /// At least one query stat was recorded for this cohort.
    Observed,
}

/// Latency bucket for grouped query telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PostgresEnum)]
pub enum QueryLatencyBucket {
    /// Query latency was below 1 millisecond.
    Lt1Ms,
    /// Query latency was at least 1 ms and below 10 ms.
    Lt10Ms,
    /// Query latency was at least 10 ms and below 100 ms.
    Lt100Ms,
    /// Query latency was at least 100 ms and below 1 second.
    Lt1S,
    /// Query latency was at least 1 second.
    Gte1S,
    /// Older or coarse samples did not declare a bucket explicitly.
    Unspecified,
}

/// SQL-visible readiness for a query explain stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PostgresEnum)]
pub enum QueryExplainStatus {
    /// The stage can run with the current catalog and source-table shape.
    Ready,
    /// The stage is a conservative fallback rather than an optimized path.
    Fallback,
    /// The row describes a deterministic policy or planner setting.
    Policy,
}

/// Lifecycle status for a PostgreSQL index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PostgresEnum)]
pub enum IndexLifecycleStatus {
    /// The index is live, ready, and valid.
    Ready,
    /// The index exists but is not ready or valid yet.
    Building,
    /// The index is not live or has otherwise failed validity checks.
    Invalid,
}

/// Diagnostic class for a PostgreSQL index serving path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PostgresEnum)]
pub enum IndexDiagnosticStatus {
    /// The index is ready for the serving path represented by its access method.
    Ready,
    /// The index or required statistics exist but are not ready for serving.
    IndexNotReady,
    /// PostgreSQL catalog state indicates a failed or corrupt index path.
    IndexCorrupt,
    /// The index access method is not a pgContext serving index.
    UnsupportedAccessMethod,
}

/// Recall check outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PostgresEnum)]
pub enum RecallCheckStatus {
    /// Candidate recall meets or exceeds the requested minimum.
    Passing,
    /// Candidate recall is below the requested minimum.
    Failing,
    /// The exact result set is empty, so recall is vacuously complete.
    EmptyExact,
}

/// Availability status for an index memory estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PostgresEnum)]
pub enum IndexMemoryEstimateStatus {
    /// The estimate uses catalog row counts and observed vector dimensions.
    Projected,
    /// The index access method does not expose pgContext memory estimates.
    UnsupportedAccessMethod,
    /// Required catalog statistics or vector dimensions are not available.
    UnavailableStatistics,
}

/// Optimization readiness for a pgContext collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PostgresEnum)]
pub enum OptimizationStatus {
    /// A matching pgContext HNSW index is present for a registered vector.
    Indexed,
    /// The collection can use exact table-backed retrieval but has no HNSW index.
    ExactOnly,
    /// Required source-table or vector registration artifacts are missing.
    MissingArtifacts,
    /// Stored catalog metadata no longer resolves to PostgreSQL objects.
    StaleCatalog,
}

/// Maintenance recommendation for an index and its owning table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PostgresEnum)]
pub enum VacuumAdviceStatus {
    /// Current PostgreSQL statistics do not indicate immediate maintenance.
    Healthy,
    /// Dead heap tuples are visible to PostgreSQL statistics.
    VacuumRecommended,
    /// Index tuple estimates are unavailable and statistics should be refreshed.
    AnalyzeRecommended,
    /// The index access method does not expose pgContext vacuum advice.
    UnsupportedAccessMethod,
}

/// Index advisor recommendation category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PostgresEnum)]
pub enum IndexAdvisorRecommendation {
    /// Existing indexes and statistics are sufficient for this advisor row.
    NoAction,
    /// Add a B-tree index for an ordinary registered filter column.
    CreateBtreeIndex,
    /// Add a GIN index for a JSONB registered filter column.
    CreateGinIndex,
    /// Refresh PostgreSQL statistics before making planner decisions.
    AnalyzeTable,
    /// Avoid materializing very broad candidate sets.
    AvoidCandidateMaterialization,
    /// Review HNSW and quantization settings for indexed serving.
    TuneHnswSettings,
}

/// Collection telemetry rollup state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PostgresEnum)]
pub enum TelemetryStatus {
    /// The collection has active points and required query artifacts.
    Active,
    /// The collection has required query artifacts but no active points.
    Empty,
    /// Required source-table or vector registration artifacts are missing.
    MissingArtifacts,
    /// Stored catalog metadata no longer resolves to PostgreSQL objects.
    StaleCatalog,
}
