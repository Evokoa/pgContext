//! PostgreSQL ports for registered lexical and fuzzy candidate execution.
//!
//! Every path renders from the canonical document expression owned by
//! [`crate::lexical_catalog`]. The exact path evaluates the complete
//! invoker-visible corpus or fails closed; the indexed path performs a bounded
//! candidate probe that is never treated as authoritative, and the recheck
//! stage rereads current source rows under MVCC and RLS before final scoring.

use context_core::{ConfigurationRevision, PointId, ScoreOrder, SourceAuthority, SourceKey};
use context_query::{
    Candidate, CandidateBranch, CandidateDiagnostics, CandidatePage, CandidateSourceKind,
    FilterCandidateBatch, FuzzyMode, FuzzyQuery, HydratedCandidate, LexicalBooleanOperator,
    LexicalQuery, PortBudget, QueryError, QueryIr, QueryKind, Result, SourceReadiness,
};
use pgrx::datum::DatumWithOid;
use pgrx::prelude::*;
use std::mem::size_of;

use super::{port_failure, spi_column, spi_optional_result_column, spi_point_id, sql_limit};
use crate::lexical_catalog::{
    LexicalIndexAm, PreparedFuzzySource, PreparedLexicalSource, prepare_fuzzy_source,
    prepare_lexical_source, quote_literal,
};
use crate::table_search::quote_identifier;

/// Serving strategy selected for one registered lexical or fuzzy source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LexicalStrategy {
    /// Complete exact evaluation of the invoker-visible corpus.
    Exact,
    /// Bounded probe through an attached GIN or GiST index.
    Indexed {
        /// Access method backing the attached index.
        access_method: LexicalIndexAm,
        /// Whether the access method returns lossy candidates.
        lossy: bool,
    },
}

impl LexicalStrategy {
    pub(crate) fn attached(index: Option<&crate::lexical_catalog::LexicalIndexBinding>) -> Self {
        index.map_or(Self::Exact, |index| Self::Indexed {
            access_method: index.access_method,
            lossy: index.is_lossy,
        })
    }

    const fn is_indexed(self) -> bool {
        matches!(self, Self::Indexed { .. })
    }

    /// Returns bounded, content-free work units distinguishing a lossy recheck.
    const fn work_units(self) -> u32 {
        match self {
            Self::Exact => 1,
            Self::Indexed { lossy: false, .. } => 2,
            Self::Indexed { lossy: true, .. } => 3,
        }
    }

    pub(crate) const fn lexical_label(self) -> &'static str {
        match self {
            Self::Exact => "lexical_exact",
            Self::Indexed {
                access_method: LexicalIndexAm::Gin,
                ..
            } => "lexical_gin",
            Self::Indexed {
                access_method: LexicalIndexAm::Gist,
                ..
            } => "lexical_gist",
        }
    }

    const fn fuzzy_label(self) -> &'static str {
        match self {
            Self::Exact => "fuzzy_exact",
            Self::Indexed {
                access_method: LexicalIndexAm::Gin,
                ..
            } => "fuzzy_gin",
            Self::Indexed {
                access_method: LexicalIndexAm::Gist,
                ..
            } => "fuzzy_gist",
        }
    }
}

/// Bound SQL parameter carried by a rendered lexical statement.
#[derive(Clone, Debug)]
enum LexicalParam {
    Text(String),
    Int(i32),
    Big(i64),
    Double(f64),
    BigArray(Vec<i64>),
}

/// Accumulates bound parameters while rendering lexical SQL.
struct BoundParams(Vec<LexicalParam>);

impl BoundParams {
    const fn new() -> Self {
        Self(Vec::new())
    }

    fn push(&mut self, param: LexicalParam) -> String {
        self.0.push(param);
        format!("${}", self.0.len())
    }

    fn text(&mut self, value: &str) -> String {
        self.push(LexicalParam::Text(value.to_owned()))
    }

    fn big(&mut self, value: usize, stage: &'static str) -> Result<String> {
        Ok(self.push(LexicalParam::Big(sql_limit(value, stage)?)))
    }

    fn as_datums(&self) -> Vec<DatumWithOid<'_>> {
        self.0
            .iter()
            .map(|param| match param {
                LexicalParam::Text(value) => value.as_str().into(),
                LexicalParam::Int(value) => (*value).into(),
                LexicalParam::Big(value) => (*value).into(),
                LexicalParam::Double(value) => (*value).into(),
                LexicalParam::BigArray(values) => values.as_slice().into(),
            })
            .collect()
    }
}

/// Compiled `tsquery` plus the optional document weight restriction.
struct LexicalRendering {
    tsquery: String,
    weight_labels: Option<String>,
}

impl LexicalRendering {
    /// Applies the registered weight restriction to a document expression.
    fn restrict(&self, document: &str) -> String {
        match &self.weight_labels {
            None => document.to_owned(),
            Some(labels) => format!("pg_catalog.ts_filter({document}, ARRAY[{labels}])"),
        }
    }
}

fn render_lexical(
    source: &PreparedLexicalSource,
    query: &LexicalQuery,
    row_tsquery_ref: &str,
    params: &mut BoundParams,
) -> Result<LexicalRendering> {
    let (query, weight_labels) = match query {
        LexicalQuery::WeightRestricted { query, weights } => {
            let labels = weights
                .weights()
                .into_iter()
                .map(|weight| format!("{}::\"char\"", quote_literal(&weight.label().to_string())))
                .collect::<Vec<_>>()
                .join(", ");
            (query.as_ref(), Some(labels))
        }
        other => (other, None),
    };
    Ok(LexicalRendering {
        tsquery: compile_tsquery(source, query, row_tsquery_ref, params)?,
        weight_labels,
    })
}

fn compile_tsquery(
    source: &PreparedLexicalSource,
    query: &LexicalQuery,
    row_tsquery_ref: &str,
    params: &mut BoundParams,
) -> Result<String> {
    let configuration = source.configuration_sql();
    let rendered = match query {
        LexicalQuery::Plain(text) => format!(
            "pg_catalog.plainto_tsquery({configuration}, {})",
            params.text(text.as_str())
        ),
        LexicalQuery::Structured(text) => format!(
            "pg_catalog.to_tsquery({configuration}, {})",
            params.text(text.as_str())
        ),
        LexicalQuery::Phrase(text) => format!(
            "pg_catalog.phraseto_tsquery({configuration}, {})",
            params.text(text.as_str())
        ),
        LexicalQuery::WebSearch(text) => format!(
            "pg_catalog.websearch_to_tsquery({configuration}, {})",
            params.text(text.as_str())
        ),
        LexicalQuery::Prefix(term) => format!(
            "pg_catalog.to_tsquery({configuration}, {} OPERATOR(pg_catalog.||) ':*'::text)",
            params.text(term.as_str())
        ),
        LexicalQuery::Distance {
            left,
            right,
            distance,
        } => {
            let left = params.text(left.as_str());
            let right = params.text(right.as_str());
            let distance = params.push(LexicalParam::Int(i32::from(*distance)));
            format!(
                "pg_catalog.tsquery_phrase(\
                 pg_catalog.plainto_tsquery({configuration}, {left}), \
                 pg_catalog.plainto_tsquery({configuration}, {right}), \
                 {distance})"
            )
        }
        LexicalQuery::Boolean { operator, clauses } => {
            let mut rendered = Vec::with_capacity(clauses.len());
            for clause in clauses {
                rendered.push(compile_tsquery(source, clause, row_tsquery_ref, params)?);
            }
            match operator {
                LexicalBooleanOperator::And => {
                    format!("({})", rendered.join(" OPERATOR(pg_catalog.&&) "))
                }
                LexicalBooleanOperator::Or => {
                    format!("({})", rendered.join(" OPERATOR(pg_catalog.||) "))
                }
                LexicalBooleanOperator::Not => {
                    let clause = rendered.first().ok_or_else(|| QueryError::PortFailure {
                        stage: "lexical_candidate_source",
                        message: "negation requires exactly one clause".to_owned(),
                    })?;
                    format!("(OPERATOR(pg_catalog.!!) {clause})")
                }
            }
        }
        LexicalQuery::WeightRestricted { .. } => {
            return Err(QueryError::PortFailure {
                stage: "lexical_candidate_source",
                message: "weight restriction is only valid at the lexical query root".to_owned(),
            });
        }
        LexicalQuery::RegisteredTsQuery(name) => {
            let Some((registered_name, _)) = &source.row_tsquery else {
                return Err(QueryError::PortFailure {
                    stage: "lexical_candidate_source",
                    message: format!(
                        "lexical source {} has no registered row tsquery",
                        source.source_name
                    ),
                });
            };
            if registered_name != name.as_str() {
                return Err(QueryError::PortFailure {
                    stage: "lexical_candidate_source",
                    message: format!("registered row tsquery is not {}", name.as_str()),
                });
            }
            row_tsquery_ref.to_owned()
        }
    };
    Ok(rendered)
}

fn rank_sql(source: &PreparedLexicalSource, document: &str, tsquery: &str) -> String {
    let [d, c, b, a] = source.rank_weights.as_array();
    format!(
        "{function}(ARRAY[{d}::real, {c}::real, {b}::real, {a}::real], \
         {document}, {tsquery}, {normalization})::double precision",
        function = source.ranker.function_name(),
        normalization = source.normalization.get(),
    )
}

fn point_id_array(candidates: &[Candidate], stage: &'static str) -> Result<String> {
    let mut ids = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let point_id =
            i64::try_from(candidate.point_id().get()).map_err(|_| QueryError::PortFailure {
                stage,
                message: "point ID exceeds PostgreSQL bigint".to_owned(),
            })?;
        ids.push(point_id.to_string());
    }
    Ok(format!("ARRAY[{}]::bigint[]", ids.join(", ")))
}

fn filter_point_id_predicate(
    filter: Option<&FilterCandidateBatch>,
    params: &mut BoundParams,
    points_alias: &str,
    stage: &'static str,
    max_memory_bytes: usize,
) -> Result<String> {
    let Some(filter) = filter else {
        return Ok(String::new());
    };
    if filter.point_ids().is_empty() {
        return Ok("AND false".to_owned());
    }
    let point_count = filter.point_ids().len();
    // Peak admission includes the retained Rust vector, PostgreSQL's datum and
    // null builder arrays, and the completed bigint[] datum. The five-value
    // multiplier conservatively covers those simultaneous per-element slots;
    // the fixed reserve covers ArrayBuildState, varlena, Vec, enum, and palloc
    // headers. `as_datums` borrows this slice, so it creates no second Rust
    // vector.
    let projected_bytes = lexical_filter_parameter_memory(point_count)?;
    if projected_bytes > max_memory_bytes {
        return Err(QueryError::WorkBudgetExceeded {
            budget: "lexical_filter_parameter_memory",
            actual: projected_bytes,
            maximum: max_memory_bytes,
        });
    }
    let mut ids = Vec::with_capacity(point_count);
    for point_id in filter.point_ids() {
        let point_id = i64::try_from(point_id.get()).map_err(|_| QueryError::PortFailure {
            stage,
            message: "filter point ID exceeds PostgreSQL bigint".to_owned(),
        })?;
        ids.push(point_id);
    }
    let ids = params.push(LexicalParam::BigArray(ids));
    Ok(format!(
        "AND {points_alias}.point_id = ANY ({ids}::bigint[])"
    ))
}

pub(crate) fn lexical_filter_parameter_memory(point_count: usize) -> Result<usize> {
    point_count
        .checked_mul(size_of::<i64>())
        .and_then(|bytes| bytes.checked_mul(5))
        .and_then(|bytes| bytes.checked_add(size_of::<LexicalParam>()))
        .and_then(|bytes| bytes.checked_add(512))
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "lexical_filter_parameter_memory",
        })
}

/// Registered lexical candidate source prepared once per execution.
#[derive(Clone, Debug)]
pub(super) struct CompositeLexicalSource {
    prepared: PreparedLexicalSource,
    strategy: LexicalStrategy,
}

impl CompositeLexicalSource {
    pub(super) fn prepare(collection_id: i64, query: &QueryIr) -> Result<Self> {
        let QueryKind::Lexical { source, .. } = query.kind() else {
            return Err(QueryError::PortFailure {
                stage: "lexical_candidate_source",
                message: "lexical adapter requires a lexical query".to_owned(),
            });
        };
        let prepared = prepare_lexical_source(collection_id, source.as_str())?;
        let strategy = LexicalStrategy::attached(prepared.index.as_ref());
        Ok(Self { prepared, strategy })
    }

    pub(super) const fn readiness(&self) -> SourceReadiness {
        match self.strategy {
            LexicalStrategy::Exact => SourceReadiness::Exact,
            LexicalStrategy::Indexed { .. } => SourceReadiness::Ready,
        }
    }

    pub(super) fn candidate_limit(&self, query: &QueryIr, remaining: usize) -> usize {
        match self.strategy {
            LexicalStrategy::Exact => query.limit().min(remaining),
            LexicalStrategy::Indexed { .. } => crate::settings::lexical_candidate_budget_from_guc()
                .max(query.limit())
                .min(remaining),
        }
    }

    pub(super) fn candidates(
        &self,
        query: &QueryIr,
        filter: Option<&FilterCandidateBatch>,
        limit: usize,
        budget: PortBudget,
    ) -> Result<CandidatePage> {
        let lexical_query = lexical_query_of(query, "lexical_candidate_source")?;
        let (rows, scored_count, exhausted) = match self.strategy {
            LexicalStrategy::Exact => self.exact_rows(
                lexical_query,
                filter,
                limit,
                budget.max_comparisons(),
                budget.max_memory_bytes(),
            )?,
            LexicalStrategy::Indexed { .. } => self.indexed_rows(
                lexical_query,
                filter,
                limit,
                budget.max_comparisons(),
                budget.max_memory_bytes(),
            )?,
        };
        let candidates = build_candidates(
            &rows,
            CandidateBranch::Lexical,
            CandidateSourceKind::Lexical,
            self.prepared.registration_revision,
            self.strategy,
        )?;
        Ok(
            CandidatePage::with_scored_count(candidates, scored_count, exhausted)
                .with_strategy(self.strategy.lexical_label())
                .with_expansion_count(usize::from(self.strategy.is_indexed())),
        )
    }

    fn exact_rows(
        &self,
        query: &LexicalQuery,
        filter: Option<&FilterCandidateBatch>,
        limit: usize,
        max_comparisons: usize,
        max_memory_bytes: usize,
    ) -> Result<LexicalRows> {
        const STAGE: &str = "lexical_candidate_source";
        let mut params = BoundParams::new();
        let collection = params.push(LexicalParam::Big(self.prepared.collection_id));
        let rendering = render_lexical(&self.prepared, query, "visible.row_query", &mut params)?;
        let probe = params.big(max_comparisons.saturating_add(1), STAGE)?;
        let visible_limit = params.big(max_comparisons, STAGE)?;
        let result_limit = params.big(limit, STAGE)?;
        let restricted = rendering.restrict("visible.document");
        let filter_sql =
            filter_point_id_predicate(filter, &mut params, "points", STAGE, max_memory_bytes)?;
        let rank = rank_sql(&self.prepared, &restricted, &rendering.tsquery);
        let row_query_select = self
            .prepared
            .row_tsquery
            .as_ref()
            .map_or_else(String::new, |(_, column)| {
                format!(", source.{} AS row_query", quote_identifier(column))
            });
        let sql = format!(
            "WITH visible AS MATERIALIZED (
                 SELECT points.point_id,
                        points.source_key,
                        {document} AS document{row_query_select}
                   FROM pgcontext._visible_collection_points AS points
                   JOIN {table} AS source ON source.id::text = points.source_key
                  WHERE points.collection_id = {collection}
                    AND points.deleted_at IS NULL
                    {filter_sql}
                  LIMIT {probe}
             ),
             admission AS (SELECT count(*)::bigint AS visible_count FROM visible),
             ranked AS MATERIALIZED (
                 SELECT visible.point_id,
                        visible.source_key,
                        {rank} AS score
                   FROM visible
                   CROSS JOIN admission
                  WHERE admission.visible_count <= {visible_limit}
                    AND {restricted} OPERATOR(pg_catalog.@@) {tsquery}
                  ORDER BY score DESC, visible.point_id ASC
                  LIMIT {result_limit}
             )
             SELECT ranked.point_id,
                    ranked.source_key,
                    ranked.score,
                    admission.visible_count
               FROM admission
               LEFT JOIN ranked ON true
              ORDER BY ranked.score DESC NULLS LAST, ranked.point_id ASC",
            document = self.prepared.document_sql(),
            table = self.prepared.qualified_table(),
            tsquery = rendering.tsquery,
        );
        let (rows, visible_count) = run_scored_rows(&sql, &params, limit.max(1))?;
        if visible_count > max_comparisons {
            return Err(QueryError::WorkBudgetExceeded {
                budget: "candidate_comparisons",
                actual: visible_count,
                maximum: max_comparisons,
            });
        }
        Ok((rows, visible_count, true))
    }

    fn indexed_rows(
        &self,
        query: &LexicalQuery,
        filter: Option<&FilterCandidateBatch>,
        limit: usize,
        max_comparisons: usize,
        max_memory_bytes: usize,
    ) -> Result<LexicalRows> {
        const STAGE: &str = "lexical_candidate_source";
        let mut params = BoundParams::new();
        let collection = params.push(LexicalParam::Big(self.prepared.collection_id));
        let row_tsquery = self.prepared.row_tsquery.as_ref().map_or_else(
            || "NULL::pg_catalog.tsquery".to_owned(),
            |(_, column)| format!("source.{}", quote_identifier(column)),
        );
        let rendering = render_lexical(&self.prepared, query, &row_tsquery, &mut params)?;
        let allowance = limit.min(max_comparisons);
        let probe = params.big(allowance.saturating_add(1), STAGE)?;
        // The probe predicate is the same expression the outer rank and the
        // recheck stage use. It deliberately is not relaxed to the unrestricted
        // document: `ts_filter` removes lexemes, and removing a lexeme can make
        // a negated clause become true, so a weight-restricted match is *not* a
        // subset of the unrestricted match. Probing the unrestricted document
        // would silently drop rows the exact path returns.
        let restricted = rendering.restrict(self.prepared.document_sql());
        let rank = rank_sql(&self.prepared, "candidates.document", "candidates.tsquery");
        let filter_sql =
            filter_point_id_predicate(filter, &mut params, "points", STAGE, max_memory_bytes)?;
        let sql = format!(
            "WITH candidates AS (
                 SELECT points.point_id,
                        points.source_key,
                        {restricted} AS document,
                        {tsquery} AS tsquery
                   FROM pgcontext._visible_collection_points AS points
                   JOIN {table} AS source ON source.id::text = points.source_key
                  WHERE points.collection_id = {collection}
                    AND points.deleted_at IS NULL
                    {filter_sql}
                    AND {restricted} OPERATOR(pg_catalog.@@) {tsquery}
                  LIMIT {probe}
             )
             SELECT candidates.point_id,
                    candidates.source_key,
                    {rank} AS score,
                    0::bigint
               FROM candidates
              ORDER BY score DESC, candidates.point_id ASC",
            table = self.prepared.qualified_table(),
            tsquery = rendering.tsquery,
        );
        // No predicate is applied after the bounded CTE, so the returned row
        // count *is* the probe cardinality and the exhaustion decision cannot
        // mistake a filtered page for a complete one.
        let (mut rows, _) = run_scored_rows(&sql, &params, allowance.saturating_add(1))?;
        let scored_count = rows.len();
        let exhausted = scored_count <= allowance;
        rows.truncate(allowance);
        Ok((rows, scored_count, exhausted))
    }

    pub(super) fn recheck(
        &self,
        query: &QueryIr,
        candidates: &[Candidate],
        limit: usize,
    ) -> Result<Vec<HydratedCandidate>> {
        const STAGE: &str = "lexical_source_recheck";
        let lexical_query = lexical_query_of(query, STAGE)?;
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let ids = point_id_array(candidates, STAGE)?;
        let mut params = BoundParams::new();
        let collection = params.push(LexicalParam::Big(self.prepared.collection_id));
        let row_tsquery = self.prepared.row_tsquery.as_ref().map_or_else(
            || "NULL::pg_catalog.tsquery".to_owned(),
            |(_, column)| format!("source.{}", quote_identifier(column)),
        );
        let rendering = render_lexical(&self.prepared, lexical_query, &row_tsquery, &mut params)?;
        let result_limit = params.big(limit, STAGE)?;
        let restricted = rendering.restrict(self.prepared.document_sql());
        let rank = rank_sql(&self.prepared, &restricted, &rendering.tsquery);
        let sql = format!(
            "SELECT points.point_id,
                    points.source_key,
                    {rank} AS score,
                    0::bigint
               FROM pg_catalog.unnest({ids}) AS requested(point_id)
               JOIN pgcontext._visible_collection_points AS points
                 ON points.collection_id = {collection}
                AND points.point_id = requested.point_id
                AND points.deleted_at IS NULL
               JOIN {table} AS source ON source.id::text = points.source_key
              WHERE {restricted} OPERATOR(pg_catalog.@@) {tsquery}
              ORDER BY score DESC, points.point_id ASC
              LIMIT {result_limit}",
            table = self.prepared.qualified_table(),
            tsquery = rendering.tsquery,
        );
        let (rows, _) = run_scored_rows(&sql, &params, limit.max(1))?;
        hydrate(rows)
    }
}

/// Registered fuzzy candidate source prepared once per execution.
#[derive(Clone, Debug)]
pub(super) struct CompositeFuzzySource {
    prepared: PreparedFuzzySource,
    strategy: LexicalStrategy,
}

impl CompositeFuzzySource {
    pub(super) fn prepare(collection_id: i64, query: &QueryIr) -> Result<Self> {
        let QueryKind::Fuzzy { source, .. } = query.kind() else {
            return Err(QueryError::PortFailure {
                stage: "fuzzy_candidate_source",
                message: "fuzzy adapter requires a fuzzy query".to_owned(),
            });
        };
        let prepared = prepare_fuzzy_source(collection_id, source.as_str())?;
        let strategy = LexicalStrategy::attached(prepared.index.as_ref());
        Ok(Self { prepared, strategy })
    }

    pub(super) const fn readiness(&self) -> SourceReadiness {
        match self.strategy {
            LexicalStrategy::Exact => SourceReadiness::Exact,
            LexicalStrategy::Indexed { .. } => SourceReadiness::Ready,
        }
    }

    pub(super) fn candidate_limit(&self, query: &QueryIr, remaining: usize) -> usize {
        match self.strategy {
            LexicalStrategy::Exact => query.limit().min(remaining),
            LexicalStrategy::Indexed { .. } => crate::settings::lexical_candidate_budget_from_guc()
                .max(query.limit())
                .min(remaining),
        }
    }

    pub(super) fn candidates(
        &self,
        query: &QueryIr,
        filter: Option<&FilterCandidateBatch>,
        limit: usize,
        budget: PortBudget,
    ) -> Result<CandidatePage> {
        let fuzzy_query = fuzzy_query_of(query, "fuzzy_candidate_source")?;
        let (rows, scored_count, exhausted) = match self.strategy {
            LexicalStrategy::Exact => self.exact_rows(
                fuzzy_query,
                filter,
                limit,
                budget.max_comparisons(),
                budget.max_memory_bytes(),
            )?,
            LexicalStrategy::Indexed { .. } => self.indexed_rows(
                fuzzy_query,
                filter,
                limit,
                budget.max_comparisons(),
                budget.max_memory_bytes(),
            )?,
        };
        let candidates = build_candidates(
            &rows,
            CandidateBranch::Fuzzy,
            CandidateSourceKind::Fuzzy,
            self.prepared.registration_revision,
            self.strategy,
        )?;
        Ok(
            CandidatePage::with_scored_count(candidates, scored_count, exhausted)
                .with_strategy(self.strategy.fuzzy_label())
                .with_expansion_count(usize::from(self.strategy.is_indexed())),
        )
    }

    fn similarity_sql(&self, mode: FuzzyMode, haystack: &str, needle: &str) -> String {
        let function = self.prepared.similarity_function(mode.function_name());
        match mode {
            FuzzyMode::Similarity => format!("{function}({haystack}, {needle})::double precision"),
            FuzzyMode::WordSimilarity | FuzzyMode::StrictWordSimilarity => {
                format!("{function}({needle}, {haystack})::double precision")
            }
        }
    }

    fn exact_rows(
        &self,
        query: &FuzzyQuery,
        filter: Option<&FilterCandidateBatch>,
        limit: usize,
        max_comparisons: usize,
        max_memory_bytes: usize,
    ) -> Result<LexicalRows> {
        const STAGE: &str = "fuzzy_candidate_source";
        let mut params = BoundParams::new();
        let collection = params.push(LexicalParam::Big(self.prepared.collection_id));
        let needle = params.text(query.text().as_str());
        let similarity = self.similarity_sql(
            query.mode(),
            &self.prepared.text_sql(Some("source")),
            &needle,
        );
        let threshold = params.push(LexicalParam::Double(query.threshold().get()));
        let probe = params.big(max_comparisons.saturating_add(1), STAGE)?;
        let visible_limit = params.big(max_comparisons, STAGE)?;
        let result_limit = params.big(limit, STAGE)?;
        let filter_sql =
            filter_point_id_predicate(filter, &mut params, "points", STAGE, max_memory_bytes)?;
        let sql = format!(
            "WITH visible AS MATERIALIZED (
                 SELECT points.point_id,
                        points.source_key,
                        {similarity} AS score
                   FROM pgcontext._visible_collection_points AS points
                   JOIN {table} AS source ON source.id::text = points.source_key
                  WHERE points.collection_id = {collection}
                    AND points.deleted_at IS NULL
                    {filter_sql}
                  LIMIT {probe}
             ),
             admission AS (SELECT count(*)::bigint AS visible_count FROM visible),
             ranked AS MATERIALIZED (
                 SELECT visible.point_id, visible.source_key, visible.score
                   FROM visible
                   CROSS JOIN admission
                  WHERE admission.visible_count <= {visible_limit}
                    AND visible.score >= {threshold}
                  ORDER BY visible.score DESC, visible.point_id ASC
                  LIMIT {result_limit}
             )
             SELECT ranked.point_id,
                    ranked.source_key,
                    ranked.score,
                    admission.visible_count
               FROM admission
               LEFT JOIN ranked ON true
              ORDER BY ranked.score DESC NULLS LAST, ranked.point_id ASC",
            table = self.prepared.qualified_table(),
        );
        let (rows, visible_count) = run_scored_rows(&sql, &params, limit.max(1))?;
        if visible_count > max_comparisons {
            return Err(QueryError::WorkBudgetExceeded {
                budget: "candidate_comparisons",
                actual: visible_count,
                maximum: max_comparisons,
            });
        }
        Ok((rows, visible_count, true))
    }

    fn indexed_rows(
        &self,
        query: &FuzzyQuery,
        filter: Option<&FilterCandidateBatch>,
        limit: usize,
        max_comparisons: usize,
        max_memory_bytes: usize,
    ) -> Result<LexicalRows> {
        const STAGE: &str = "fuzzy_candidate_source";
        let mut params = BoundParams::new();
        let collection = params.push(LexicalParam::Big(self.prepared.collection_id));
        let needle = params.text(query.text().as_str());
        let haystack = self.prepared.text_sql(Some("source"));
        let operator = self.trigram_operator(query.mode());
        let predicate = match query.mode() {
            FuzzyMode::Similarity => format!("{haystack} {operator} {needle}"),
            FuzzyMode::WordSimilarity | FuzzyMode::StrictWordSimilarity => {
                format!("{needle} {operator} {haystack}")
            }
        };
        let similarity = self.similarity_sql(query.mode(), "candidates.haystack", &needle);
        let allowance = limit.min(max_comparisons);
        let probe = params.big(allowance.saturating_add(1), STAGE)?;
        let filter_sql =
            filter_point_id_predicate(filter, &mut params, "points", STAGE, max_memory_bytes)?;
        let sql = format!(
            "WITH candidates AS (
                 SELECT points.point_id,
                        points.source_key,
                        {haystack} AS haystack
                   FROM pgcontext._visible_collection_points AS points
                   JOIN {table} AS source ON source.id::text = points.source_key
                  WHERE points.collection_id = {collection}
                    AND points.deleted_at IS NULL
                    {filter_sql}
                    AND {predicate}
                  LIMIT {probe}
             )
             SELECT candidates.point_id,
                    candidates.source_key,
                    {similarity} AS score,
                    0::bigint
               FROM candidates
              ORDER BY score DESC, candidates.point_id ASC",
            table = self.prepared.qualified_table(),
        );
        let (mut rows, _) = self.with_scoped_threshold(query, || {
            run_scored_rows(&sql, &params, allowance.saturating_add(1))
        })?;
        let scored_count = rows.len();
        let exhausted = scored_count <= allowance;
        rows.truncate(allowance);
        Ok((rows, scored_count, exhausted))
    }

    fn trigram_operator(&self, mode: FuzzyMode) -> String {
        let symbol = match mode {
            FuzzyMode::Similarity => "%",
            FuzzyMode::WordSimilarity => "<%",
            FuzzyMode::StrictWordSimilarity => "<<%",
        };
        format!(
            "OPERATOR({}.{symbol})",
            quote_identifier(&self.prepared.trgm_schema_name)
        )
    }

    /// Runs `action` with the mode's trigram threshold set, then restores it.
    ///
    /// The previous value is captured first and reapplied by an RAII guard, so
    /// the normal and `Err` paths both restore it. The setting is written with
    /// `set_config(..., is_local => true)`, so a PostgreSQL error that unwinds
    /// past the guard is discarded by transaction-local GUC rollback instead of
    /// leaking into the surrounding transaction.
    fn with_scoped_threshold<T>(
        &self,
        query: &FuzzyQuery,
        action: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let guard = ScopedThreshold::arm(query.mode(), query.threshold().get())?;
        let outcome = action();
        guard.restore();
        outcome
    }

    pub(super) fn recheck(
        &self,
        query: &QueryIr,
        candidates: &[Candidate],
        limit: usize,
    ) -> Result<Vec<HydratedCandidate>> {
        const STAGE: &str = "fuzzy_source_recheck";
        let fuzzy_query = fuzzy_query_of(query, STAGE)?;
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let ids = point_id_array(candidates, STAGE)?;
        let mut params = BoundParams::new();
        let collection = params.push(LexicalParam::Big(self.prepared.collection_id));
        let needle = params.text(fuzzy_query.text().as_str());
        let similarity = self.similarity_sql(
            fuzzy_query.mode(),
            &self.prepared.text_sql(Some("source")),
            &needle,
        );
        let threshold = params.push(LexicalParam::Double(fuzzy_query.threshold().get()));
        let result_limit = params.big(limit, STAGE)?;
        let sql = format!(
            "SELECT points.point_id,
                    points.source_key,
                    {similarity} AS score,
                    0::bigint
               FROM pg_catalog.unnest({ids}) AS requested(point_id)
               JOIN pgcontext._visible_collection_points AS points
                 ON points.collection_id = {collection}
                AND points.point_id = requested.point_id
                AND points.deleted_at IS NULL
               JOIN {table} AS source ON source.id::text = points.source_key
              WHERE {similarity} >= {threshold}
              ORDER BY score DESC, points.point_id ASC
              LIMIT {result_limit}",
            table = self.prepared.qualified_table(),
        );
        let (rows, _) = run_scored_rows(&sql, &params, limit.max(1))?;
        hydrate(rows)
    }
}

fn lexical_query_of<'a>(query: &'a QueryIr, stage: &'static str) -> Result<&'a LexicalQuery> {
    match query.kind() {
        QueryKind::Lexical { query, .. } => Ok(query),
        _ => Err(QueryError::PortFailure {
            stage,
            message: "lexical adapter requires a lexical query".to_owned(),
        }),
    }
}

fn fuzzy_query_of<'a>(query: &'a QueryIr, stage: &'static str) -> Result<&'a FuzzyQuery> {
    match query.kind() {
        QueryKind::Fuzzy { query, .. } => Ok(query),
        _ => Err(QueryError::PortFailure {
            stage,
            message: "fuzzy adapter requires a fuzzy query".to_owned(),
        }),
    }
}

fn build_candidates(
    rows: &[ScoredRow],
    branch: CandidateBranch,
    source: CandidateSourceKind,
    registration_revision: i64,
    strategy: LexicalStrategy,
) -> Result<Vec<Candidate>> {
    let revision = u64::try_from(registration_revision)
        .ok()
        .and_then(ConfigurationRevision::new);
    let mut candidates = Vec::with_capacity(rows.len());
    for (rank, (point_id, _, score)) in rows.iter().enumerate() {
        let mut provenance = super::candidate_provenance(
            *point_id,
            branch,
            source,
            ScoreOrder::HigherIsBetter,
            SourceAuthority::PostgreSqlRow,
        )?;
        if let Some(revision) = revision {
            provenance = provenance.with_configuration(revision);
        }
        candidates.push(
            Candidate::new(*point_id, *score, provenance)?
                .with_exact_score(*score)?
                .with_diagnostics(CandidateDiagnostics::new(
                    u32::try_from(rank).unwrap_or(u32::MAX),
                    strategy.work_units(),
                )),
        );
    }
    Ok(candidates)
}

/// Restores a scoped `pg_trgm` threshold when the probe leaves its scope.
struct ScopedThreshold {
    setting: &'static str,
    previous: String,
    restored: bool,
}

impl ScopedThreshold {
    fn arm(mode: FuzzyMode, threshold: f64) -> Result<Self> {
        let setting = mode.threshold_setting();
        let previous = Spi::get_one_with_args::<String>(
            "SELECT pg_catalog.current_setting($1, true)",
            &[setting.into()],
        )
        .map_err(|error| port_failure("fuzzy_candidate_source", error))?
        .unwrap_or_else(|| mode.default_threshold().to_string());
        set_threshold(setting, &threshold.to_string())?;
        Ok(Self {
            setting,
            previous,
            restored: false,
        })
    }

    fn restore(mut self) {
        self.restored = true;
        // A failed restore must not mask the probe's own outcome; the setting is
        // transaction-local, so PostgreSQL still discards it at transaction end.
        let _ = set_threshold(self.setting, &self.previous);
    }
}

impl Drop for ScopedThreshold {
    fn drop(&mut self) {
        if self.restored || std::thread::panicking() {
            // During a PostgreSQL error unwind SPI is not safe to re-enter;
            // transaction-local GUC rollback restores the value instead.
            return;
        }
        let _ = set_threshold(self.setting, &self.previous);
    }
}

fn set_threshold(setting: &str, value: &str) -> Result<()> {
    Spi::get_one_with_args::<String>(
        "SELECT pg_catalog.set_config($1, $2, true)",
        &[setting.into(), value.into()],
    )
    .map(|_| ())
    .map_err(|error| port_failure("fuzzy_candidate_source", error))
}

type ScoredRow = (PointId, String, f64);
type LexicalRows = (Vec<ScoredRow>, usize, bool);

fn run_scored_rows(
    sql: &str,
    params: &BoundParams,
    limit: usize,
) -> Result<(Vec<ScoredRow>, usize)> {
    const STAGE: &str = "lexical_candidate_source";
    let args = params.as_datums();
    Spi::connect(|client| {
        let spi_rows = client
            .select(sql, Some(sql_limit(limit.max(1), STAGE)?), &args)
            .map_err(|error| port_failure(STAGE, error))?;
        let mut rows = Vec::with_capacity(limit.max(1));
        let mut visible_count = 0_usize;
        for row in spi_rows {
            let observed = spi_column::<i64>(&row, 4, STAGE)?;
            let observed = usize::try_from(observed).map_err(|_| QueryError::PortFailure {
                stage: STAGE,
                message: "visible row count exceeds usize".to_owned(),
            })?;
            visible_count = visible_count.max(observed);
            if spi_optional_result_column::<i64>(&row, 1, STAGE)?.is_none() {
                continue;
            }
            rows.push((
                spi_point_id(&row, 1, STAGE)?,
                spi_column::<String>(&row, 2, STAGE)?,
                spi_column::<f64>(&row, 3, STAGE)?,
            ));
        }
        Ok((rows, visible_count))
    })
}

fn hydrate(rows: Vec<ScoredRow>) -> Result<Vec<HydratedCandidate>> {
    rows.into_iter()
        .map(|(point_id, source_key, score)| {
            HydratedCandidate::new(point_id, SourceKey::new(source_key)?, score)
        })
        .collect()
}

/// Returns bounded `ts_headline` fragments for already-retrieved points.
///
/// Point count, option bytes, and source-document bytes are admitted before
/// PostgreSQL builds any markup. The accumulated output size is a hard cap on
/// the returned result: it is enforced while reading the response, because
/// PostgreSQL has no way to bound `ts_headline` output before generating it.
/// Callers must sanitize the returned markup for their own output context.
pub(crate) fn lexical_headline_rows(
    source: &PreparedLexicalSource,
    query: &LexicalQuery,
    point_ids: &[i64],
    options: &str,
    max_source_bytes: usize,
    max_output_bytes: usize,
) -> Result<Vec<(i64, String)>> {
    const STAGE: &str = "lexical_headline";
    let Some(document_text) = source.headline_text_sql() else {
        return Err(QueryError::PortFailure {
            stage: STAGE,
            message: "stored-vector lexical sources carry no raw text to highlight".to_owned(),
        });
    };
    let mut params = BoundParams::new();
    let collection = params.push(LexicalParam::Big(source.collection_id));
    let row_tsquery = source.row_tsquery.as_ref().map_or_else(
        || "NULL::pg_catalog.tsquery".to_owned(),
        |(_, column)| format!("source.{}", quote_identifier(column)),
    );
    let rendering = render_lexical(source, query, &row_tsquery, &mut params)?;
    let options_param = params.text(options);
    let source_budget = params.big(max_source_bytes, STAGE)?;
    let ids = point_ids
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "WITH selected AS MATERIALIZED (
             SELECT points.point_id,
                    {document_text} AS source_text,
                    {tsquery} AS tsquery
               FROM pg_catalog.unnest(ARRAY[{ids}]::bigint[]) AS requested(point_id)
               JOIN pgcontext._visible_collection_points AS points
                 ON points.collection_id = {collection}
                AND points.point_id = requested.point_id
                AND points.deleted_at IS NULL
               JOIN {table} AS source ON source.id::text = points.source_key
         ),
         admission AS (
             SELECT coalesce(
                        pg_catalog.sum(pg_catalog.octet_length(selected.source_text)), 0
                    )::bigint AS source_bytes
               FROM selected
         )
         SELECT selected.point_id,
                CASE WHEN admission.source_bytes <= {source_budget}
                     THEN pg_catalog.ts_headline(
                              {configuration}, selected.source_text, selected.tsquery,
                              {options_param}
                          )
                     ELSE ''::text
                END AS headline,
                admission.source_bytes <= {source_budget} AS admitted
           FROM selected
           CROSS JOIN admission
          ORDER BY selected.point_id ASC",
        configuration = source.configuration_sql(),
        table = source.qualified_table(),
        tsquery = rendering.tsquery,
    );
    let args = params.as_datums();
    Spi::connect(|client| {
        let spi_rows = client
            .select(&sql, Some(sql_limit(point_ids.len(), STAGE)?), &args)
            .map_err(|error| port_failure(STAGE, error))?;
        let mut rows = Vec::with_capacity(point_ids.len());
        let mut output_bytes = 0_usize;
        for row in spi_rows {
            // The admission decision is reported by the statement itself, so an
            // empty result (missing, invisible, or deleted points) is never
            // misreported as a source-byte budget failure.
            if !spi_column::<bool>(&row, 3, STAGE)? {
                return Err(QueryError::WorkBudgetExceeded {
                    budget: "lexical_headline_source_bytes",
                    actual: max_source_bytes.saturating_add(1),
                    maximum: max_source_bytes,
                });
            }
            let point_id = spi_column::<i64>(&row, 1, STAGE)?;
            let headline = spi_column::<String>(&row, 2, STAGE)?;
            output_bytes =
                output_bytes
                    .checked_add(headline.len())
                    .ok_or(QueryError::ArithmeticOverflow {
                        operation: "lexical_headline_output_projection",
                    })?;
            if output_bytes > max_output_bytes {
                return Err(QueryError::WorkBudgetExceeded {
                    budget: "lexical_headline_output_bytes",
                    actual: output_bytes,
                    maximum: max_output_bytes,
                });
            }
            rows.push((point_id, headline));
        }
        Ok(rows)
    })
}
