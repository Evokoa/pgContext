//! Typed filter parsing and predicate planning for pgContext.
//!
//! The filter crate converts JSON-facing filter input into typed predicate
//! structures. It never accepts raw SQL fragments from callers.

use core::fmt;
use core::str::FromStr;
use std::collections::BTreeMap;

use context_core::policy::{
    MAX_FILTER_DEPTH, MAX_FILTER_KEY_BYTES, MAX_FILTER_NODES, MAX_FILTER_PATH_DEPTH,
};
use context_core::{Error as CoreError, SqlIdentifier};
use serde_json::{Map, Number, Value};

mod sql;

pub use sql::{
    SqlParameter, SqlParameterBinding, SqlParameterType, SqlPredicatePlan, render_sql_predicate,
};

/// Result type used by `context-filter`.
pub type Result<T> = core::result::Result<T, FilterError>;

/// Errors produced while parsing filter JSON into a typed AST.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum FilterError {
    /// The JSON input is malformed.
    #[error("invalid filter JSON: {0}")]
    InvalidJson(String),

    /// The root filter object contains no conditions.
    #[error("filter must contain at least one condition")]
    EmptyFilter,

    /// An object contains an unsupported field.
    #[error("unknown filter field: {field}")]
    UnknownField {
        /// Unsupported field name.
        field: String,
    },

    /// An expected JSON object was another type.
    #[error("expected filter object at {path}")]
    ExpectedObject {
        /// JSON path where the object was expected.
        path: String,
    },

    /// An expected JSON array was another type.
    #[error("expected filter array at {path}")]
    ExpectedArray {
        /// JSON path where the array was expected.
        path: String,
    },

    /// A required condition key is missing or malformed.
    #[error("invalid filter key: {0}")]
    InvalidKey(String),

    /// A field condition omitted its predicate.
    #[error("missing predicate for filter key: {key}")]
    MissingPredicate {
        /// Field key whose condition had no predicate.
        key: FilterKey,
    },

    /// A predicate has an unsupported shape.
    #[error("invalid predicate for filter key {key}: {reason}")]
    InvalidPredicate {
        /// Field key whose predicate failed validation.
        key: FilterKey,
        /// Stable validation reason.
        reason: &'static str,
    },

    /// A configured parser budget was exceeded.
    #[error("{budget} exceeded: {actual} > {max}")]
    BudgetExceeded {
        /// Budget category.
        budget: &'static str,
        /// Observed value.
        actual: usize,
        /// Maximum accepted value.
        max: usize,
    },

    /// A filter key does not match any registered payload field.
    #[error("unknown payload field: {key}")]
    UnknownPayloadField {
        /// Unknown filter key.
        key: FilterKey,
    },

    /// A payload field key was registered more than once.
    #[error("duplicate payload field: {key}")]
    DuplicatePayloadField {
        /// Duplicate filter key.
        key: FilterKey,
    },

    /// A registered SQL column identifier failed validation.
    #[error("{0}")]
    InvalidColumn(#[from] CoreError),

    /// A registered JSONB path failed validation.
    #[error("invalid JSONB path: {reason}: {path:?}")]
    InvalidJsonbPath {
        /// Path segments that failed validation.
        path: Vec<String>,
        /// Stable validation reason.
        reason: &'static str,
    },
}

/// Qdrant-style boolean filter AST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    /// Conditions that must match.
    pub must: Vec<Condition>,
    /// Conditions where at least one should match.
    pub should: Vec<Condition>,
    /// Conditions that must not match.
    pub must_not: Vec<Condition>,
}

impl Filter {
    fn is_empty(&self) -> bool {
        self.must.is_empty() && self.should.is_empty() && self.must_not.is_empty()
    }
}

/// One filter condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Condition {
    /// Field-level predicate.
    Field {
        /// Field key to resolve later against registered columns or JSONB paths.
        key: FilterKey,
        /// Predicate to apply to the resolved field.
        predicate: Predicate,
    },
    /// Nested boolean filter.
    Nested(Filter),
}

/// Field-level predicate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Predicate {
    /// Equality-style match predicate.
    Match(MatchValue),
    /// Numeric or lexical range predicate.
    Range {
        /// Strict greater-than bound.
        gt: Option<RangeBound>,
        /// Inclusive greater-than bound.
        gte: Option<RangeBound>,
        /// Strict less-than bound.
        lt: Option<RangeBound>,
        /// Inclusive less-than bound.
        lte: Option<RangeBound>,
    },
    /// Null check.
    IsNull(bool),
    /// Empty-array/string check.
    IsEmpty(bool),
}

/// Match predicate value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchValue {
    /// One scalar value.
    Value(PayloadValue),
    /// Any of the provided scalar values.
    Any(Vec<PayloadValue>),
    /// None of the provided scalar values.
    Except(Vec<PayloadValue>),
}

/// JSON scalar value accepted in a filter predicate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadValue {
    /// JSON null.
    Null,
    /// JSON boolean.
    Bool(bool),
    /// JSON number.
    Number(Number),
    /// JSON string.
    String(String),
}

impl From<&str> for PayloadValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<String> for PayloadValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<bool> for PayloadValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<i64> for PayloadValue {
    fn from(value: i64) -> Self {
        Self::Number(Number::from(value))
    }
}

/// Range bound value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeBound {
    /// Integer bound.
    Integer(i64),
    /// Floating-point bound, stored as the original JSON number.
    Float(Number),
    /// String bound.
    String(String),
}

impl From<i64> for RangeBound {
    fn from(value: i64) -> Self {
        Self::Integer(value)
    }
}

impl From<&str> for RangeBound {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

/// Validated filter field key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FilterKey(String);

impl FilterKey {
    /// Returns the stored filter key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for FilterKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for FilterKey {
    type Err = FilterError;

    fn from_str(value: &str) -> Result<Self> {
        validate_filter_key(value).map(|()| Self(value.to_owned()))
    }
}

/// Validated JSONB path segments for a registered filter field.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JsonbPath(Vec<String>);

impl JsonbPath {
    /// Validates and stores JSONB path segments.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError::InvalidJsonbPath`] when a segment is empty or
    /// contains NUL bytes. Returns [`FilterError::BudgetExceeded`] when the
    /// path exceeds the configured path-depth budget.
    pub fn new(path: impl IntoIterator<Item = impl Into<String>>) -> Result<Self> {
        let path = path.into_iter().map(Into::into).collect::<Vec<_>>();
        validate_jsonb_path(&path)?;
        Ok(Self(path))
    }

    /// Returns the stored path segments.
    #[must_use]
    pub fn segments(&self) -> &[String] {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RegisteredField {
    Column(SqlIdentifier),
    JsonbPath {
        column: SqlIdentifier,
        path: JsonbPath,
    },
}

/// Registry of filter keys that may be resolved against ordinary table columns.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FieldRegistry {
    fields: BTreeMap<FilterKey, RegisteredField>,
}

impl FieldRegistry {
    /// Returns a builder for registering filterable fields.
    #[must_use]
    pub fn builder() -> FieldRegistryBuilder {
        FieldRegistryBuilder::default()
    }

    /// Resolves every filter field against this registry.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError::UnknownPayloadField`] when a user-provided filter
    /// key is not registered.
    pub fn resolve_filter(&self, filter: &Filter) -> Result<ResolvedFilter> {
        Ok(ResolvedFilter {
            must: self.resolve_conditions(&filter.must)?,
            should: self.resolve_conditions(&filter.should)?,
            must_not: self.resolve_conditions(&filter.must_not)?,
        })
    }

    fn resolve_conditions(&self, conditions: &[Condition]) -> Result<Vec<ResolvedCondition>> {
        conditions
            .iter()
            .map(|condition| self.resolve_condition(condition))
            .collect()
    }

    fn resolve_condition(&self, condition: &Condition) -> Result<ResolvedCondition> {
        match condition {
            Condition::Field { key, predicate } => {
                let Some(field) = self.fields.get(key) else {
                    return Err(FilterError::UnknownPayloadField { key: key.clone() });
                };
                Ok(ResolvedCondition::Field {
                    field: ResolvedField::from_registered(key, field),
                    predicate: ResolvedPredicate::from(predicate),
                })
            }
            Condition::Nested(filter) => self.resolve_filter(filter).map(ResolvedCondition::Nested),
        }
    }
}

/// Builder for [`FieldRegistry`].
#[derive(Debug, Clone, Default)]
pub struct FieldRegistryBuilder {
    fields: BTreeMap<FilterKey, RegisteredField>,
}

impl FieldRegistryBuilder {
    /// Registers one filter key as an ordinary SQL column.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError::DuplicatePayloadField`] when `key` was already
    /// registered. Returns [`FilterError::InvalidColumn`] when either the key or
    /// column identifier fails validation.
    pub fn register_column(
        mut self,
        key: impl AsRef<str>,
        column: impl Into<String>,
    ) -> Result<Self> {
        let key = FilterKey::from_str(key.as_ref())?;
        let column = SqlIdentifier::new(column.into())?;
        if self.fields.contains_key(&key) {
            return Err(FilterError::DuplicatePayloadField { key });
        }
        self.fields.insert(key, RegisteredField::Column(column));
        Ok(self)
    }

    /// Registers one filter key as a path inside a JSONB column.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError::DuplicatePayloadField`] when `key` was already
    /// registered. Returns [`FilterError::InvalidColumn`] when the column
    /// identifier fails validation. Returns [`FilterError::InvalidJsonbPath`] or
    /// [`FilterError::BudgetExceeded`] when the JSONB path fails validation.
    pub fn register_jsonb_path(
        mut self,
        key: impl AsRef<str>,
        column: impl Into<String>,
        path: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self> {
        let key = FilterKey::from_str(key.as_ref())?;
        let column = SqlIdentifier::new(column.into())?;
        let path = JsonbPath::new(path)?;
        if self.fields.contains_key(&key) {
            return Err(FilterError::DuplicatePayloadField { key });
        }
        self.fields
            .insert(key, RegisteredField::JsonbPath { column, path });
        Ok(self)
    }

    /// Builds the immutable field registry.
    #[must_use]
    pub fn build(self) -> FieldRegistry {
        FieldRegistry {
            fields: self.fields,
        }
    }
}

/// Filter whose field keys have been resolved against a registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFilter {
    /// Conditions that must match.
    pub must: Vec<ResolvedCondition>,
    /// Conditions where at least one should match.
    pub should: Vec<ResolvedCondition>,
    /// Conditions that must not match.
    pub must_not: Vec<ResolvedCondition>,
}

/// One resolved filter condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedCondition {
    /// Predicate against a resolved field.
    Field {
        /// Resolved field.
        field: ResolvedField,
        /// Predicate to apply.
        predicate: ResolvedPredicate,
    },
    /// Nested boolean filter.
    Nested(ResolvedFilter),
}

impl ResolvedCondition {
    /// Returns the resolved field for field conditions.
    #[must_use]
    pub const fn field(&self) -> Option<&ResolvedField> {
        match self {
            Self::Field { field, .. } => Some(field),
            Self::Nested(_) => None,
        }
    }
}

/// Resolved field target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedField {
    /// Ordinary source-table column.
    Column {
        /// User-facing filter key.
        key: FilterKey,
        /// Validated SQL column identifier.
        column: SqlIdentifier,
    },
    /// Path inside a JSONB source-table column.
    JsonbPath {
        /// User-facing filter key.
        key: FilterKey,
        /// Validated SQL JSONB column identifier.
        column: SqlIdentifier,
        /// Validated JSONB path segments.
        path: JsonbPath,
    },
}

impl ResolvedField {
    fn from_registered(key: &FilterKey, field: &RegisteredField) -> Self {
        match field {
            RegisteredField::Column(column) => Self::Column {
                key: key.clone(),
                column: column.clone(),
            },
            RegisteredField::JsonbPath { column, path } => Self::JsonbPath {
                key: key.clone(),
                column: column.clone(),
                path: path.clone(),
            },
        }
    }
}

/// Predicate attached to a resolved field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedPredicate {
    /// Equality-style match predicate.
    Match(MatchValue),
    /// Numeric or lexical range predicate.
    Range {
        /// Strict greater-than bound.
        gt: Option<RangeBound>,
        /// Inclusive greater-than bound.
        gte: Option<RangeBound>,
        /// Strict less-than bound.
        lt: Option<RangeBound>,
        /// Inclusive less-than bound.
        lte: Option<RangeBound>,
    },
    /// Null check.
    IsNull(bool),
    /// Empty-array/string check.
    IsEmpty(bool),
}

impl From<&Predicate> for ResolvedPredicate {
    fn from(predicate: &Predicate) -> Self {
        match predicate {
            Predicate::Match(value) => Self::Match(value.clone()),
            Predicate::Range { gt, gte, lt, lte } => Self::Range {
                gt: gt.clone(),
                gte: gte.clone(),
                lt: lt.clone(),
                lte: lte.clone(),
            },
            Predicate::IsNull(value) => Self::IsNull(*value),
            Predicate::IsEmpty(value) => Self::IsEmpty(*value),
        }
    }
}

/// Parses Qdrant-style filter JSON into a typed AST.
///
/// # Errors
///
/// Returns [`FilterError`] when the input is malformed JSON, has unsupported
/// fields or predicate shapes, or exceeds parser policy limits.
pub fn parse_filter_json(input: &str) -> Result<Filter> {
    let json = serde_json::from_str::<Value>(input)
        .map_err(|error| FilterError::InvalidJson(error.to_string()))?;
    let mut budget = FilterBudget::default();
    parse_filter_value(&json, "$", 1, &mut budget)
}

#[derive(Debug, Default)]
struct FilterBudget {
    nodes: usize,
}

impl FilterBudget {
    fn count_node(&mut self) -> Result<()> {
        self.nodes += 1;
        if self.nodes > MAX_FILTER_NODES {
            return Err(FilterError::BudgetExceeded {
                budget: "filter nodes",
                actual: self.nodes,
                max: MAX_FILTER_NODES,
            });
        }
        Ok(())
    }
}

fn parse_filter_value(
    value: &Value,
    path: &str,
    depth: usize,
    budget: &mut FilterBudget,
) -> Result<Filter> {
    if depth > MAX_FILTER_DEPTH {
        return Err(FilterError::BudgetExceeded {
            budget: "filter depth",
            actual: depth,
            max: MAX_FILTER_DEPTH,
        });
    }

    let object = value
        .as_object()
        .ok_or_else(|| FilterError::ExpectedObject {
            path: path.to_owned(),
        })?;

    let mut filter = Filter {
        must: Vec::new(),
        should: Vec::new(),
        must_not: Vec::new(),
    };

    for (key, value) in object {
        match key.as_str() {
            "must" => filter.must = parse_condition_array(value, "$.must", depth, budget)?,
            "should" => {
                filter.should = parse_condition_array(value, "$.should", depth, budget)?;
            }
            "must_not" => {
                filter.must_not = parse_condition_array(value, "$.must_not", depth, budget)?;
            }
            _ => return Err(FilterError::UnknownField { field: key.clone() }),
        }
    }

    if filter.is_empty() {
        return Err(FilterError::EmptyFilter);
    }

    Ok(filter)
}

fn parse_condition_array(
    value: &Value,
    path: &str,
    depth: usize,
    budget: &mut FilterBudget,
) -> Result<Vec<Condition>> {
    let array = value.as_array().ok_or_else(|| FilterError::ExpectedArray {
        path: path.to_owned(),
    })?;

    let mut conditions = Vec::with_capacity(array.len());
    for (index, condition) in array.iter().enumerate() {
        conditions.push(parse_condition(
            condition,
            &format!("{path}[{index}]"),
            depth + 1,
            budget,
        )?);
    }
    Ok(conditions)
}

fn parse_condition(
    value: &Value,
    path: &str,
    depth: usize,
    budget: &mut FilterBudget,
) -> Result<Condition> {
    if let Some(object) = value.as_object()
        && is_nested_filter(object)
    {
        budget.count_node()?;
        return parse_filter_value(value, path, depth, budget).map(Condition::Nested);
    }

    budget.count_node()?;
    parse_field_condition(value, path)
}

fn is_nested_filter(object: &Map<String, Value>) -> bool {
    object.contains_key("must") || object.contains_key("should") || object.contains_key("must_not")
}

fn parse_field_condition(value: &Value, path: &str) -> Result<Condition> {
    let object = value
        .as_object()
        .ok_or_else(|| FilterError::ExpectedObject {
            path: path.to_owned(),
        })?;
    let key = parse_condition_key(object)?;
    let mut predicate = None;

    for (field, value) in object {
        match field.as_str() {
            "key" => {}
            "match" => predicate = Some(Predicate::Match(parse_match_value(&key, value)?)),
            "range" => predicate = Some(parse_range_predicate(&key, value)?),
            "is_null" => predicate = Some(Predicate::IsNull(parse_bool_predicate(&key, value)?)),
            "is_empty" => predicate = Some(Predicate::IsEmpty(parse_bool_predicate(&key, value)?)),
            _ => {
                return Err(FilterError::UnknownField {
                    field: field.clone(),
                });
            }
        }
    }

    let predicate = predicate.ok_or_else(|| FilterError::MissingPredicate { key: key.clone() })?;
    Ok(Condition::Field { key, predicate })
}

fn parse_condition_key(object: &Map<String, Value>) -> Result<FilterKey> {
    let Some(value) = object.get("key") else {
        return Err(FilterError::InvalidKey("missing key".to_owned()));
    };
    let Some(key) = value.as_str() else {
        return Err(FilterError::InvalidKey("key must be a string".to_owned()));
    };
    key.parse()
}

fn parse_match_value(key: &FilterKey, value: &Value) -> Result<MatchValue> {
    if let Some(object) = value.as_object() {
        if object.len() != 1 {
            return Err(invalid_predicate(
                key,
                "match object must contain one operator",
            ));
        }
        if let Some(value) = object.get("value") {
            return parse_payload_value(key, value).map(MatchValue::Value);
        }
        if let Some(value) = object.get("any") {
            return parse_payload_array(key, value).map(MatchValue::Any);
        }
        if let Some(value) = object.get("except") {
            return parse_payload_array(key, value).map(MatchValue::Except);
        }
        return Err(invalid_predicate(key, "unsupported match operator"));
    }

    parse_payload_value(key, value).map(MatchValue::Value)
}

fn parse_payload_array(key: &FilterKey, value: &Value) -> Result<Vec<PayloadValue>> {
    let Some(array) = value.as_array() else {
        return Err(invalid_predicate(key, "match list must be an array"));
    };
    if array.is_empty() {
        return Err(invalid_predicate(key, "match list must not be empty"));
    }
    array
        .iter()
        .map(|value| parse_payload_value(key, value))
        .collect()
}

fn parse_payload_value(key: &FilterKey, value: &Value) -> Result<PayloadValue> {
    match value {
        Value::Null => Ok(PayloadValue::Null),
        Value::Bool(value) => Ok(PayloadValue::Bool(*value)),
        Value::Number(value) => Ok(PayloadValue::Number(value.clone())),
        Value::String(value) => Ok(PayloadValue::String(value.clone())),
        Value::Array(_) | Value::Object(_) => {
            Err(invalid_predicate(key, "match value must be a JSON scalar"))
        }
    }
}

fn parse_range_predicate(key: &FilterKey, value: &Value) -> Result<Predicate> {
    let Some(object) = value.as_object() else {
        return Err(invalid_predicate(key, "range must be an object"));
    };

    let mut gt = None;
    let mut gte = None;
    let mut lt = None;
    let mut lte = None;
    for (field, value) in object {
        let bound = parse_range_bound(key, value)?;
        match field.as_str() {
            "gt" => gt = Some(bound),
            "gte" => gte = Some(bound),
            "lt" => lt = Some(bound),
            "lte" => lte = Some(bound),
            _ => {
                return Err(FilterError::UnknownField {
                    field: field.clone(),
                });
            }
        }
    }

    if gt.is_none() && gte.is_none() && lt.is_none() && lte.is_none() {
        return Err(invalid_predicate(
            key,
            "range must contain at least one bound",
        ));
    }

    Ok(Predicate::Range { gt, gte, lt, lte })
}

fn parse_range_bound(key: &FilterKey, value: &Value) -> Result<RangeBound> {
    match value {
        Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                Ok(RangeBound::Integer(integer))
            } else {
                Ok(RangeBound::Float(number.clone()))
            }
        }
        Value::String(value) => Ok(RangeBound::String(value.clone())),
        _ => Err(invalid_predicate(
            key,
            "range bound must be a number or string",
        )),
    }
}

fn parse_bool_predicate(key: &FilterKey, value: &Value) -> Result<bool> {
    value
        .as_bool()
        .ok_or_else(|| invalid_predicate(key, "predicate value must be a boolean"))
}

fn validate_filter_key(value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(FilterError::InvalidKey("key must not be empty".to_owned()));
    }
    if value.len() > MAX_FILTER_KEY_BYTES {
        return Err(FilterError::BudgetExceeded {
            budget: "filter key bytes",
            actual: value.len(),
            max: MAX_FILTER_KEY_BYTES,
        });
    }
    if value.contains('\0') {
        return Err(FilterError::InvalidKey(
            "key must not contain NUL bytes".to_owned(),
        ));
    }

    let segments = value.split('.').collect::<Vec<_>>();
    if segments.len() > MAX_FILTER_PATH_DEPTH {
        return Err(FilterError::BudgetExceeded {
            budget: "filter path depth",
            actual: segments.len(),
            max: MAX_FILTER_PATH_DEPTH,
        });
    }
    if segments.iter().any(|segment| segment.is_empty()) {
        return Err(FilterError::InvalidKey(
            "key path segments must not be empty".to_owned(),
        ));
    }

    Ok(())
}

fn validate_jsonb_path(path: &[String]) -> Result<()> {
    if path.is_empty() {
        return Err(FilterError::InvalidJsonbPath {
            path: path.to_vec(),
            reason: "path must not be empty",
        });
    }
    if path.len() > MAX_FILTER_PATH_DEPTH {
        return Err(FilterError::BudgetExceeded {
            budget: "JSONB path depth",
            actual: path.len(),
            max: MAX_FILTER_PATH_DEPTH,
        });
    }
    if path.iter().any(|segment| segment.is_empty()) {
        return Err(FilterError::InvalidJsonbPath {
            path: path.to_vec(),
            reason: "path segments must not be empty",
        });
    }
    if path.iter().any(|segment| segment.contains('\0')) {
        return Err(FilterError::InvalidJsonbPath {
            path: path.to_vec(),
            reason: "path segments must not contain NUL bytes",
        });
    }

    Ok(())
}

fn invalid_predicate(key: &FilterKey, reason: &'static str) -> FilterError {
    FilterError::InvalidPredicate {
        key: key.clone(),
        reason,
    }
}

/// Returns the package version compiled into this crate.
#[must_use]
pub const fn crate_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
