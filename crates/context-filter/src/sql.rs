//! SQL predicate rendering from resolved filter predicates.

use crate::{
    MatchValue, PayloadValue, RangeBound, ResolvedCondition, ResolvedField, ResolvedFilter,
    ResolvedPredicate,
};

/// Rendered SQL predicate with ordered parameter values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlPredicatePlan {
    /// SQL predicate text containing positional placeholders.
    pub sql: String,
    /// Values to bind to each placeholder in SQL order.
    pub parameters: Vec<SqlParameter>,
    /// SQL type intent for each placeholder in SQL order.
    pub parameter_types: Vec<SqlParameterType>,
}

impl SqlPredicatePlan {
    /// Returns the ordered placeholder binding contract for the predicate.
    #[must_use]
    pub fn bindings(&self) -> Vec<SqlParameterBinding> {
        self.parameters
            .iter()
            .cloned()
            .zip(self.parameter_types.iter().copied())
            .enumerate()
            .map(|(index, (value, parameter_type))| SqlParameterBinding {
                index: index + 1,
                parameter_type,
                value,
            })
            .collect()
    }
}

/// Parameter value referenced by a rendered SQL predicate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SqlParameter {
    /// One JSON-compatible scalar predicate value.
    JsonValue(PayloadValue),
    /// JSON-compatible scalar values for `ANY` or `ALL` predicates.
    JsonArray(Vec<PayloadValue>),
    /// One range comparison bound.
    RangeBound(RangeBound),
    /// JSONB path segments bound as a `text[]` value.
    JsonbPath(Vec<String>),
}

/// SQL type intent for one rendered predicate placeholder.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum SqlParameterType {
    /// Let PostgreSQL infer the type from the surrounding predicate.
    InferFromSql,
    /// Bind the value as `jsonb`.
    Jsonb,
    /// Bind the value as a `jsonb[]` array.
    JsonbArray,
    /// Bind the value as a `text[]` array.
    TextArray,
}

/// One ordered placeholder binding for a rendered predicate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlParameterBinding {
    /// One-based SQL placeholder index.
    pub index: usize,
    /// SQL type intent for this binding.
    pub parameter_type: SqlParameterType,
    /// Value to bind.
    pub value: SqlParameter,
}

/// Renders a resolved filter into a SQL predicate plan.
///
/// The renderer only accepts fields that have already been resolved through a
/// [`crate::FieldRegistry`]. User predicate values and JSONB path segments are
/// represented as parameters instead of being interpolated into SQL text.
#[must_use]
pub fn render_sql_predicate(filter: &ResolvedFilter) -> SqlPredicatePlan {
    let mut renderer = PredicateRenderer::default();
    let sql = renderer.render_filter(filter);
    SqlPredicatePlan {
        sql,
        parameters: renderer.parameters,
        parameter_types: renderer.parameter_types,
    }
}

#[derive(Debug, Default)]
struct PredicateRenderer {
    parameters: Vec<SqlParameter>,
    parameter_types: Vec<SqlParameterType>,
}

impl PredicateRenderer {
    fn render_filter(&mut self, filter: &ResolvedFilter) -> String {
        let mut terms = Vec::new();

        terms.extend(
            filter
                .must
                .iter()
                .map(|condition| self.render_condition(condition)),
        );

        if !filter.should.is_empty() {
            let should = filter
                .should
                .iter()
                .map(|condition| self.render_condition(condition))
                .collect::<Vec<_>>()
                .join(" OR ");
            terms.push(format!("({should})"));
        }

        terms.extend(
            filter
                .must_not
                .iter()
                .map(|condition| format!("NOT {}", self.render_condition(condition))),
        );

        if terms.is_empty() {
            return "(TRUE)".to_owned();
        }
        if terms.len() == 1 {
            return terms.remove(0);
        }

        format!("({})", terms.join(" AND "))
    }

    fn render_condition(&mut self, condition: &ResolvedCondition) -> String {
        match condition {
            ResolvedCondition::Field { field, predicate } => {
                let target = self.render_field(field);
                self.render_predicate(&target, predicate)
            }
            ResolvedCondition::Nested(filter) => self.render_filter(filter),
        }
    }

    fn render_field(&mut self, field: &ResolvedField) -> RenderedField {
        match field {
            ResolvedField::Column { column, .. } => RenderedField::Column(column.to_string()),
            ResolvedField::JsonbPath { column, path, .. } => {
                let placeholder = self.push_parameter(
                    SqlParameter::JsonbPath(path.segments().iter().map(String::to_owned).collect()),
                    SqlParameterType::TextArray,
                );
                RenderedField::Jsonb(format!("({column} #> {placeholder}::text[])"))
            }
        }
    }

    fn render_predicate(
        &mut self,
        target: &RenderedField,
        predicate: &ResolvedPredicate,
    ) -> String {
        match predicate {
            ResolvedPredicate::Match(value) => self.render_match(target, value),
            ResolvedPredicate::Range { gt, gte, lt, lte } => {
                self.render_range(target, gt.as_ref(), gte.as_ref(), lt.as_ref(), lte.as_ref())
            }
            ResolvedPredicate::IsNull(value) => {
                let operator = if *value { "IS NULL" } else { "IS NOT NULL" };
                format!("({} {operator})", target.sql())
            }
            ResolvedPredicate::IsEmpty(value) => {
                let operator = if *value { "=" } else { "<>" };
                let placeholder = self.push_parameter(
                    SqlParameter::JsonValue(PayloadValue::String(String::new())),
                    target.parameter_type(),
                );
                format!("({} {operator} {placeholder})", target.sql())
            }
        }
    }

    fn render_match(&mut self, target: &RenderedField, value: &MatchValue) -> String {
        match value {
            MatchValue::Value(value) => {
                let placeholder = self.push_parameter(
                    SqlParameter::JsonValue(value.clone()),
                    target.parameter_type(),
                );
                format!("({} = {})", target.sql(), target.cast_value(&placeholder))
            }
            MatchValue::Any(values) => {
                let placeholder = self.push_parameter(
                    SqlParameter::JsonArray(values.clone()),
                    target.array_parameter_type(),
                );
                format!(
                    "({} = ANY({}))",
                    target.sql(),
                    target.cast_array(&placeholder)
                )
            }
            MatchValue::Except(values) => {
                let placeholder = self.push_parameter(
                    SqlParameter::JsonArray(values.clone()),
                    target.array_parameter_type(),
                );
                format!(
                    "({} <> ALL({}))",
                    target.sql(),
                    target.cast_array(&placeholder)
                )
            }
        }
    }

    fn render_range(
        &mut self,
        target: &RenderedField,
        gt: Option<&RangeBound>,
        gte: Option<&RangeBound>,
        lt: Option<&RangeBound>,
        lte: Option<&RangeBound>,
    ) -> String {
        let mut terms = Vec::new();
        if let Some(bound) = gt {
            terms.push(self.render_range_bound(target, ">", bound));
        }
        if let Some(bound) = gte {
            terms.push(self.render_range_bound(target, ">=", bound));
        }
        if let Some(bound) = lt {
            terms.push(self.render_range_bound(target, "<", bound));
        }
        if let Some(bound) = lte {
            terms.push(self.render_range_bound(target, "<=", bound));
        }
        format!("({})", terms.join(" AND "))
    }

    fn render_range_bound(
        &mut self,
        target: &RenderedField,
        operator: &'static str,
        bound: &RangeBound,
    ) -> String {
        let placeholder = self.push_parameter(
            SqlParameter::RangeBound(bound.clone()),
            target.parameter_type(),
        );
        format!("{} {operator} {placeholder}", target.sql())
    }

    fn push_parameter(
        &mut self,
        parameter: SqlParameter,
        parameter_type: SqlParameterType,
    ) -> String {
        self.parameters.push(parameter);
        self.parameter_types.push(parameter_type);
        format!("${}", self.parameters.len())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RenderedField {
    Column(String),
    Jsonb(String),
}

impl RenderedField {
    fn sql(&self) -> &str {
        match self {
            Self::Column(sql) | Self::Jsonb(sql) => sql,
        }
    }

    fn cast_value(&self, placeholder: &str) -> String {
        match self {
            Self::Column(_) => placeholder.to_owned(),
            Self::Jsonb(_) => format!("{placeholder}::jsonb"),
        }
    }

    fn cast_array(&self, placeholder: &str) -> String {
        match self {
            Self::Column(_) => placeholder.to_owned(),
            Self::Jsonb(_) => format!("{placeholder}::jsonb[]"),
        }
    }

    const fn parameter_type(&self) -> SqlParameterType {
        match self {
            Self::Column(_) => SqlParameterType::InferFromSql,
            Self::Jsonb(_) => SqlParameterType::Jsonb,
        }
    }

    const fn array_parameter_type(&self) -> SqlParameterType {
        match self {
            Self::Column(_) => SqlParameterType::InferFromSql,
            Self::Jsonb(_) => SqlParameterType::JsonbArray,
        }
    }
}
