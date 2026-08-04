//! Typed PostgreSQL-native lexical and fuzzy retrieval semantics.
//!
//! This module owns every bound, identifier rule, canonical name, and JSON
//! shape used by the registered lexical and fuzzy query leaves. PostgreSQL
//! owns parsing, dictionaries, matching, ranking, collation, and index
//! selection; the adapter crate resolves catalog identity and renders SQL from
//! the validated values defined here.
//!
//! Weight restriction is a document-side restriction implemented natively by
//! `ts_filter`, so [`LexicalQuery::WeightRestricted`] is only valid at the root
//! of a lexical query. Every other form composes freely inside the bounded
//! Boolean and distance constructors.

use serde_json::{Map, Value};

use crate::{QueryError, Result};

/// Maximum UTF-8 bytes accepted in one lexical or fuzzy text input.
pub const MAX_LEXICAL_TEXT_BYTES: usize = 4_096;
/// Maximum typed nodes accepted in one lexical query tree.
pub const MAX_LEXICAL_QUERY_NODES: usize = 256;
/// Maximum nesting depth accepted in one lexical query tree.
pub const MAX_LEXICAL_QUERY_DEPTH: usize = 16;
/// Maximum UTF-8 bytes accepted in a registered lexical identifier.
pub const MAX_LEXICAL_NAME_BYTES: usize = 63;
/// Maximum PostgreSQL phrase distance accepted by `tsquery_phrase`.
pub const MAX_LEXICAL_PHRASE_DISTANCE: u16 = 16_384;
/// Maximum PostgreSQL rank normalization bitmask (`1|2|4|8|16|32`).
pub const MAX_LEXICAL_NORMALIZATION: u32 = 63;
/// Maximum clauses accepted by one Boolean lexical constructor.
pub const MAX_LEXICAL_BOOLEAN_CLAUSES: usize = 64;
/// Maximum registered fields in one lexical document.
pub const MAX_LEXICAL_FIELDS: usize = 16;
/// Maximum path components for one JSON or JSONB lexical field.
pub const MAX_LEXICAL_JSON_PATH_DEPTH: usize = 16;
/// Maximum point IDs accepted by one headline hydration call.
pub const MAX_LEXICAL_HEADLINE_POINTS: usize = 1_000;
/// Maximum source-document bytes admitted by one headline call.
pub const MAX_LEXICAL_HEADLINE_SOURCE_BYTES: usize = 8 * 1024 * 1024;
/// Maximum returned headline bytes across one headline call.
pub const MAX_LEXICAL_HEADLINE_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
/// Maximum `ts_headline` option bytes accepted by one headline call.
pub const MAX_LEXICAL_HEADLINE_OPTIONS_BYTES: usize = 4_096;

/// Validated registered lexical source identifier.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LexicalSourceName(String);

/// Validated registered fuzzy (trigram) source identifier.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct FuzzySourceName(String);

/// Validated registered row-`tsquery` binding identifier.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RegisteredTsQueryName(String);

macro_rules! lexical_identifier {
    ($type:ty, $field:literal) => {
        impl $type {
            /// Creates a validated registered identifier.
            ///
            /// # Errors
            ///
            /// Returns [`QueryError::InvalidInput`] when the identifier is
            /// empty, longer than [`MAX_LEXICAL_NAME_BYTES`], or contains a
            /// character outside `[a-z0-9_]` after a leading letter or
            /// underscore.
            pub fn new(name: impl Into<String>) -> Result<Self> {
                let name = name.into();
                validate_identifier($field, &name)?;
                Ok(Self(name))
            }

            /// Returns the identifier text.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

lexical_identifier!(LexicalSourceName, "lexical_source");
lexical_identifier!(FuzzySourceName, "fuzzy_source");
lexical_identifier!(RegisteredTsQueryName, "registered_tsquery");

fn validate_identifier(field: &'static str, name: &str) -> Result<()> {
    if name.is_empty() || name.len() > MAX_LEXICAL_NAME_BYTES {
        return Err(invalid(field, "must contain 1..=63 bytes"));
    }
    let mut bytes = name.bytes();
    let first = bytes.next().unwrap_or(b'0');
    if !(first == b'_' || first.is_ascii_lowercase()) {
        return Err(invalid(
            field,
            "must begin with a lowercase letter or underscore",
        ));
    }
    if !bytes.all(|byte| byte == b'_' || byte.is_ascii_lowercase() || byte.is_ascii_digit()) {
        return Err(invalid(
            field,
            "must contain only lowercase letters, digits, and underscores",
        ));
    }
    Ok(())
}

/// Bounded, non-blank lexical query text handed to PostgreSQL constructors.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LexicalText(String);

impl LexicalText {
    /// Creates bounded lexical text.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when the text is blank, exceeds
    /// [`MAX_LEXICAL_TEXT_BYTES`], or contains a NUL or control character that
    /// PostgreSQL text search cannot accept.
    pub fn new(text: impl Into<String>) -> Result<Self> {
        let text = text.into();
        if text.is_empty() || text.len() > MAX_LEXICAL_TEXT_BYTES {
            return Err(invalid("lexical_text", "must contain 1..=4096 bytes"));
        }
        if text.trim().is_empty() {
            return Err(invalid("lexical_text", "must contain a non-blank token"));
        }
        if text
            .chars()
            .any(|character| character == '\0' || (character.is_control() && character != '\n'))
        {
            return Err(invalid(
                "lexical_text",
                "must not contain NUL or control characters",
            ));
        }
        Ok(Self(text))
    }

    /// Returns the bounded text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Single prefix term rendered as a PostgreSQL `lexeme:*` query.
///
/// Prefix terms are restricted to alphanumeric characters and underscores so
/// the adapter can append the native prefix marker without any `tsquery`
/// operator reaching the parser from untrusted input.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LexicalPrefixTerm(String);

impl LexicalPrefixTerm {
    /// Creates a validated single prefix term.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when the term is empty, exceeds
    /// [`MAX_LEXICAL_TEXT_BYTES`], or contains a character other than a
    /// Unicode alphanumeric or `_`.
    pub fn new(term: impl Into<String>) -> Result<Self> {
        let term = term.into();
        if term.is_empty() || term.len() > MAX_LEXICAL_TEXT_BYTES {
            return Err(invalid("lexical_prefix", "must contain 1..=4096 bytes"));
        }
        if !term
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_')
        {
            return Err(invalid(
                "lexical_prefix",
                "must contain only alphanumeric characters and underscores",
            ));
        }
        Ok(Self(term))
    }

    /// Returns the validated prefix term.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// PostgreSQL lexeme weight label.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LexicalWeight {
    /// Highest-priority `A` weight.
    A,
    /// `B` weight.
    B,
    /// `C` weight.
    C,
    /// Lowest-priority `D` weight.
    D,
}

impl LexicalWeight {
    /// Every weight in canonical `A`-to-`D` order.
    pub const ALL: [Self; 4] = [Self::A, Self::B, Self::C, Self::D];

    /// Returns the uppercase PostgreSQL weight label.
    #[must_use]
    pub const fn label(self) -> char {
        match self {
            Self::A => 'A',
            Self::B => 'B',
            Self::C => 'C',
            Self::D => 'D',
        }
    }

    /// Returns the stable lowercase diagnostic and JSON name.
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::A => "a",
            Self::B => "b",
            Self::C => "c",
            Self::D => "d",
        }
    }

    /// Parses a stable weight name.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] for any name other than `a`, `b`,
    /// `c`, or `d`.
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "a" => Ok(Self::A),
            "b" => Ok(Self::B),
            "c" => Ok(Self::C),
            "d" => Ok(Self::D),
            _ => Err(invalid("lexical_weight", "must be one of a, b, c, or d")),
        }
    }

    const fn bit(self) -> u8 {
        match self {
            Self::A => 0b0001,
            Self::B => 0b0010,
            Self::C => 0b0100,
            Self::D => 0b1000,
        }
    }
}

/// Nonempty set of PostgreSQL lexeme weights.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LexicalWeightSet(u8);

impl LexicalWeightSet {
    /// Creates a nonempty weight set from an unordered weight list.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when the list is empty or contains
    /// a duplicate weight.
    pub fn new(weights: &[LexicalWeight]) -> Result<Self> {
        if weights.is_empty() {
            return Err(invalid(
                "lexical_weights",
                "must contain at least one weight",
            ));
        }
        let mut mask = 0_u8;
        for weight in weights {
            let bit = weight.bit();
            if mask & bit != 0 {
                return Err(invalid("lexical_weights", "must not repeat a weight"));
            }
            mask |= bit;
        }
        Ok(Self(mask))
    }

    /// Returns the contained weights in canonical `A`-to-`D` order.
    #[must_use]
    pub fn weights(self) -> Vec<LexicalWeight> {
        LexicalWeight::ALL
            .into_iter()
            .filter(|weight| self.0 & weight.bit() != 0)
            .collect()
    }

    /// Reports whether the set contains a weight.
    #[must_use]
    pub const fn contains(self, weight: LexicalWeight) -> bool {
        self.0 & weight.bit() != 0
    }
}

/// Registered native ranking function.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LexicalRanker {
    /// PostgreSQL `ts_rank`.
    TsRank,
    /// PostgreSQL `ts_rank_cd`.
    TsRankCd,
}

impl LexicalRanker {
    /// Returns the stable diagnostic and catalog name.
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::TsRank => "ts_rank",
            Self::TsRankCd => "ts_rank_cd",
        }
    }

    /// Returns the schema-qualified PostgreSQL function name.
    #[must_use]
    pub const fn function_name(self) -> &'static str {
        match self {
            Self::TsRank => "pg_catalog.ts_rank",
            Self::TsRankCd => "pg_catalog.ts_rank_cd",
        }
    }

    /// Parses a stable ranker name.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] for an unregistered ranker name.
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "ts_rank" => Ok(Self::TsRank),
            "ts_rank_cd" => Ok(Self::TsRankCd),
            _ => Err(invalid("lexical_ranker", "must be ts_rank or ts_rank_cd")),
        }
    }
}

/// Validated PostgreSQL rank normalization bitmask.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LexicalNormalization(u32);

impl LexicalNormalization {
    /// PostgreSQL's default: no normalization.
    pub const NONE: Self = Self(0);

    /// Creates a validated normalization mask.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when the mask sets a bit outside
    /// [`MAX_LEXICAL_NORMALIZATION`].
    pub fn new(mask: u32) -> Result<Self> {
        if mask > MAX_LEXICAL_NORMALIZATION {
            return Err(invalid(
                "lexical_normalization",
                "must set only the documented 1|2|4|8|16|32 bits",
            ));
        }
        Ok(Self(mask))
    }

    /// Returns the normalization mask.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Registered `D`, `C`, `B`, `A` rank weights.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct LexicalRankWeights {
    d: f32,
    c: f32,
    b: f32,
    a: f32,
}

impl LexicalRankWeights {
    /// PostgreSQL's documented default `{0.1, 0.2, 0.4, 1.0}`.
    pub const DEFAULT: Self = Self {
        d: 0.1,
        c: 0.2,
        b: 0.4,
        a: 1.0,
    };

    /// Creates validated rank weights in PostgreSQL's `{D, C, B, A}` order.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when any weight is non-finite or
    /// outside `0.0..=1.0`.
    pub fn new(d: f32, c: f32, b: f32, a: f32) -> Result<Self> {
        for weight in [d, c, b, a] {
            if !weight.is_finite() || !(0.0..=1.0).contains(&weight) {
                return Err(invalid(
                    "lexical_rank_weights",
                    "must be finite values within 0.0..=1.0",
                ));
            }
        }
        Ok(Self { d, c, b, a })
    }

    /// Returns the weights in PostgreSQL's `{D, C, B, A}` array order.
    #[must_use]
    pub const fn as_array(self) -> [f32; 4] {
        [self.d, self.c, self.b, self.a]
    }
}

impl Default for LexicalRankWeights {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Boolean combinator applied to bounded lexical clauses.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LexicalBooleanOperator {
    /// PostgreSQL `tsquery && tsquery`.
    And,
    /// PostgreSQL `tsquery || tsquery`.
    Or,
    /// PostgreSQL `!! tsquery`.
    Not,
}

impl LexicalBooleanOperator {
    /// Returns the stable diagnostic and JSON name.
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::And => "and",
            Self::Or => "or",
            Self::Not => "not",
        }
    }

    /// Parses a stable operator name.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] for an unsupported operator name.
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "and" => Ok(Self::And),
            "or" => Ok(Self::Or),
            "not" => Ok(Self::Not),
            _ => Err(invalid(
                "lexical_boolean_operator",
                "must be and, or, or not",
            )),
        }
    }
}

/// Typed PostgreSQL-native lexical query form.
#[derive(Clone, Debug, PartialEq)]
pub enum LexicalQuery {
    /// `plainto_tsquery` over bounded raw text.
    Plain(LexicalText),
    /// `to_tsquery` over caller-supplied `tsquery` syntax.
    Structured(LexicalText),
    /// `phraseto_tsquery` over bounded raw text.
    Phrase(LexicalText),
    /// `websearch_to_tsquery` over bounded raw web-search text.
    WebSearch(LexicalText),
    /// `to_tsquery` prefix match over one validated term.
    Prefix(LexicalPrefixTerm),
    /// `tsquery_phrase` between two bounded texts at a fixed distance.
    Distance {
        /// Left phrase operand.
        left: LexicalText,
        /// Right phrase operand.
        right: LexicalText,
        /// Positive lexeme distance accepted by `tsquery_phrase`.
        distance: u16,
    },
    /// Bounded Boolean combination of lexical clauses.
    Boolean {
        /// Combinator applied to the clauses.
        operator: LexicalBooleanOperator,
        /// Owned clauses.
        clauses: Vec<LexicalQuery>,
    },
    /// Weight-restricted evaluation of a nested query.
    ///
    /// Restriction applies to the registered document through `ts_filter`, so
    /// this form is only valid at the root of a lexical query.
    WeightRestricted {
        /// Owned nested query.
        query: Box<LexicalQuery>,
        /// Document weights admitted before matching and ranking.
        weights: LexicalWeightSet,
    },
    /// Registered per-row `tsquery` column binding.
    RegisteredTsQuery(RegisteredTsQueryName),
}

impl LexicalQuery {
    /// Returns the stable form name used by diagnostics and JSON plans.
    #[must_use]
    pub const fn form_name(&self) -> &'static str {
        match self {
            Self::Plain(_) => "plain",
            Self::Structured(_) => "structured",
            Self::Phrase(_) => "phrase",
            Self::WebSearch(_) => "web_search",
            Self::Prefix(_) => "prefix",
            Self::Distance { .. } => "distance",
            Self::Boolean { .. } => "boolean",
            Self::WeightRestricted { .. } => "weight_restricted",
            Self::RegisteredTsQuery(_) => "registered_tsquery",
        }
    }

    /// Creates a validated bounded Boolean combination.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when `and`/`or` receive fewer than
    /// two clauses, `not` receives other than one clause, the clause count
    /// exceeds [`MAX_LEXICAL_BOOLEAN_CLAUSES`], or a clause is itself invalid.
    pub fn boolean(operator: LexicalBooleanOperator, clauses: Vec<Self>) -> Result<Self> {
        let query = Self::Boolean { operator, clauses };
        query.validate()?;
        Ok(query)
    }

    /// Creates a validated distance query.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when `distance` is zero or exceeds
    /// [`MAX_LEXICAL_PHRASE_DISTANCE`].
    pub fn distance(left: LexicalText, right: LexicalText, distance: u16) -> Result<Self> {
        let query = Self::Distance {
            left,
            right,
            distance,
        };
        query.validate()?;
        Ok(query)
    }

    /// Creates a validated root-level weight restriction.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when the nested query already
    /// contains a weight restriction or is itself invalid.
    pub fn weight_restricted(query: Self, weights: LexicalWeightSet) -> Result<Self> {
        let query = Self::WeightRestricted {
            query: Box::new(query),
            weights,
        };
        query.validate()?;
        Ok(query)
    }

    /// Validates structural bounds for this query tree.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when the tree exceeds the node,
    /// depth, clause, or distance bounds, or when a weight restriction appears
    /// below the root.
    pub fn validate(&self) -> Result<()> {
        let mut nodes = 0;
        validate_lexical(self, 1, &mut nodes, true)
    }

    /// Renders the canonical JSON representation of this query.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut object = Map::new();
        object.insert("form".to_owned(), Value::from(self.form_name()));
        match self {
            Self::Plain(text)
            | Self::Structured(text)
            | Self::Phrase(text)
            | Self::WebSearch(text) => {
                object.insert("text".to_owned(), Value::from(text.as_str()));
            }
            Self::Prefix(term) => {
                object.insert("term".to_owned(), Value::from(term.as_str()));
            }
            Self::Distance {
                left,
                right,
                distance,
            } => {
                object.insert("left".to_owned(), Value::from(left.as_str()));
                object.insert("right".to_owned(), Value::from(right.as_str()));
                object.insert("distance".to_owned(), Value::from(u64::from(*distance)));
            }
            Self::Boolean { operator, clauses } => {
                object.insert("operator".to_owned(), Value::from(operator.stable_name()));
                object.insert(
                    "clauses".to_owned(),
                    Value::Array(clauses.iter().map(Self::to_json).collect()),
                );
            }
            Self::WeightRestricted { query, weights } => {
                object.insert(
                    "weights".to_owned(),
                    Value::Array(
                        weights
                            .weights()
                            .into_iter()
                            .map(|weight| Value::from(weight.stable_name()))
                            .collect(),
                    ),
                );
                object.insert("query".to_owned(), query.to_json());
            }
            Self::RegisteredTsQuery(name) => {
                object.insert("name".to_owned(), Value::from(name.as_str()));
            }
        }
        Value::Object(object)
    }

    /// Parses the canonical JSON representation, rejecting unknown fields.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] for a malformed, unknown, or
    /// out-of-bounds lexical query object.
    pub fn from_json(value: &Value) -> Result<Self> {
        let query = parse_lexical_json(value, 1, &mut 0)?;
        query.validate()?;
        Ok(query)
    }
}

fn validate_lexical(
    query: &LexicalQuery,
    depth: usize,
    nodes: &mut usize,
    is_root: bool,
) -> Result<()> {
    if depth > MAX_LEXICAL_QUERY_DEPTH {
        return Err(invalid(
            "lexical_query",
            "exceeds maximum lexical nesting depth",
        ));
    }
    *nodes = nodes.saturating_add(1);
    if *nodes > MAX_LEXICAL_QUERY_NODES {
        return Err(invalid(
            "lexical_query",
            "exceeds maximum lexical node count",
        ));
    }
    match query {
        LexicalQuery::Plain(_)
        | LexicalQuery::Structured(_)
        | LexicalQuery::Phrase(_)
        | LexicalQuery::WebSearch(_)
        | LexicalQuery::Prefix(_)
        | LexicalQuery::RegisteredTsQuery(_) => Ok(()),
        LexicalQuery::Distance { distance, .. } => {
            if *distance == 0 || *distance > MAX_LEXICAL_PHRASE_DISTANCE {
                return Err(invalid(
                    "lexical_distance",
                    "must be within 1..=16384 lexemes",
                ));
            }
            Ok(())
        }
        LexicalQuery::Boolean { operator, clauses } => {
            let required = match operator {
                LexicalBooleanOperator::And | LexicalBooleanOperator::Or => 2,
                LexicalBooleanOperator::Not => 1,
            };
            if clauses.len() < required {
                return Err(invalid(
                    "lexical_clauses",
                    "and/or require at least two clauses and not requires exactly one",
                ));
            }
            if matches!(operator, LexicalBooleanOperator::Not) && clauses.len() != 1 {
                return Err(invalid(
                    "lexical_clauses",
                    "and/or require at least two clauses and not requires exactly one",
                ));
            }
            if clauses.len() > MAX_LEXICAL_BOOLEAN_CLAUSES {
                return Err(invalid(
                    "lexical_clauses",
                    "exceed the maximum Boolean clause count",
                ));
            }
            for clause in clauses {
                validate_lexical(clause, depth.saturating_add(1), nodes, false)?;
            }
            Ok(())
        }
        LexicalQuery::WeightRestricted { query, .. } => {
            if !is_root {
                return Err(invalid(
                    "lexical_weights",
                    "weight restriction is only valid at the lexical query root",
                ));
            }
            validate_lexical(query, depth.saturating_add(1), nodes, false)
        }
    }
}

fn parse_lexical_json(value: &Value, depth: usize, nodes: &mut usize) -> Result<LexicalQuery> {
    if depth > MAX_LEXICAL_QUERY_DEPTH {
        return Err(invalid(
            "lexical_query",
            "exceeds maximum lexical nesting depth",
        ));
    }
    *nodes = nodes.saturating_add(1);
    if *nodes > MAX_LEXICAL_QUERY_NODES {
        return Err(invalid(
            "lexical_query",
            "exceeds maximum lexical node count",
        ));
    }
    let object = value
        .as_object()
        .ok_or_else(|| invalid("lexical_query", "must be an object"))?;
    let form = object
        .get("form")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("lexical_query", "must declare a string form"))?;
    match form {
        "plain" | "structured" | "phrase" | "web_search" => {
            require_keys(object, &["form", "text"])?;
            let text = LexicalText::new(string_field(object, "text")?)?;
            Ok(match form {
                "plain" => LexicalQuery::Plain(text),
                "structured" => LexicalQuery::Structured(text),
                "phrase" => LexicalQuery::Phrase(text),
                _ => LexicalQuery::WebSearch(text),
            })
        }
        "prefix" => {
            require_keys(object, &["form", "term"])?;
            Ok(LexicalQuery::Prefix(LexicalPrefixTerm::new(string_field(
                object, "term",
            )?)?))
        }
        "distance" => {
            require_keys(object, &["form", "left", "right", "distance"])?;
            Ok(LexicalQuery::Distance {
                left: LexicalText::new(string_field(object, "left")?)?,
                right: LexicalText::new(string_field(object, "right")?)?,
                distance: u16_field(object, "distance")?,
            })
        }
        "boolean" => {
            require_keys(object, &["form", "operator", "clauses"])?;
            let operator = LexicalBooleanOperator::parse(string_field(object, "operator")?)?;
            let values = object
                .get("clauses")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid("lexical_clauses", "must be an array"))?;
            if values.len() > MAX_LEXICAL_BOOLEAN_CLAUSES {
                return Err(invalid(
                    "lexical_clauses",
                    "exceed the maximum Boolean clause count",
                ));
            }
            let clauses = values
                .iter()
                .map(|clause| parse_lexical_json(clause, depth.saturating_add(1), nodes))
                .collect::<Result<Vec<_>>>()?;
            Ok(LexicalQuery::Boolean { operator, clauses })
        }
        "weight_restricted" => {
            require_keys(object, &["form", "weights", "query"])?;
            let values = object
                .get("weights")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid("lexical_weights", "must be an array"))?;
            let weights = values
                .iter()
                .map(|value| {
                    LexicalWeight::parse(
                        value
                            .as_str()
                            .ok_or_else(|| invalid("lexical_weights", "must contain strings"))?,
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            let nested = object
                .get("query")
                .ok_or_else(|| invalid("lexical_query", "weight restriction requires a query"))?;
            Ok(LexicalQuery::WeightRestricted {
                query: Box::new(parse_lexical_json(nested, depth.saturating_add(1), nodes)?),
                weights: LexicalWeightSet::new(&weights)?,
            })
        }
        "registered_tsquery" => {
            require_keys(object, &["form", "name"])?;
            Ok(LexicalQuery::RegisteredTsQuery(RegisteredTsQueryName::new(
                string_field(object, "name")?,
            )?))
        }
        _ => Err(invalid("lexical_query", "declares an unsupported form")),
    }
}

/// Trigram similarity mode backed by `pg_trgm`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum FuzzyMode {
    /// `similarity` / `%` whole-string similarity.
    Similarity,
    /// `word_similarity` / `<%` similarity.
    WordSimilarity,
    /// `strict_word_similarity` / `<<%` similarity.
    StrictWordSimilarity,
}

impl FuzzyMode {
    /// Returns the stable diagnostic and JSON name.
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::Similarity => "similarity",
            Self::WordSimilarity => "word_similarity",
            Self::StrictWordSimilarity => "strict_word_similarity",
        }
    }

    /// Returns the unqualified `pg_trgm` similarity function name.
    #[must_use]
    pub const fn function_name(self) -> &'static str {
        match self {
            Self::Similarity => "similarity",
            Self::WordSimilarity => "word_similarity",
            Self::StrictWordSimilarity => "strict_word_similarity",
        }
    }

    /// Returns this mode's PostgreSQL default threshold.
    ///
    /// `pg_trgm` placeholder GUCs read as NULL until the module is loaded into
    /// the session, so a scoped probe restores this documented boot value
    /// rather than inventing one shared across modes.
    #[must_use]
    pub const fn default_threshold(self) -> f64 {
        match self {
            Self::Similarity => 0.3,
            Self::WordSimilarity => 0.6,
            Self::StrictWordSimilarity => 0.5,
        }
    }

    /// Returns the `pg_trgm` GUC that carries this mode's threshold.
    #[must_use]
    pub const fn threshold_setting(self) -> &'static str {
        match self {
            Self::Similarity => "pg_trgm.similarity_threshold",
            Self::WordSimilarity => "pg_trgm.word_similarity_threshold",
            Self::StrictWordSimilarity => "pg_trgm.strict_word_similarity_threshold",
        }
    }

    /// Parses a stable mode name.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] for an unsupported mode name.
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "similarity" => Ok(Self::Similarity),
            "word_similarity" => Ok(Self::WordSimilarity),
            "strict_word_similarity" => Ok(Self::StrictWordSimilarity),
            _ => Err(invalid(
                "fuzzy_mode",
                "must be similarity, word_similarity, or strict_word_similarity",
            )),
        }
    }
}

/// Validated trigram similarity threshold.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct FuzzyThreshold(f64);

impl FuzzyThreshold {
    /// Creates a validated threshold.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when the threshold is non-finite
    /// or outside `0.0 < threshold <= 1.0`.
    pub fn new(threshold: f64) -> Result<Self> {
        if !threshold.is_finite() || threshold <= 0.0 || threshold > 1.0 {
            return Err(invalid(
                "fuzzy_threshold",
                "must be a finite value within 0.0 < threshold <= 1.0",
            ));
        }
        Ok(Self(threshold))
    }

    /// Returns the threshold.
    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }
}

/// Typed trigram query evaluated against a registered fuzzy source.
#[derive(Clone, Debug, PartialEq)]
pub struct FuzzyQuery {
    text: LexicalText,
    mode: FuzzyMode,
    threshold: FuzzyThreshold,
}

impl FuzzyQuery {
    /// Creates a validated fuzzy query.
    #[must_use]
    pub const fn new(text: LexicalText, mode: FuzzyMode, threshold: FuzzyThreshold) -> Self {
        Self {
            text,
            mode,
            threshold,
        }
    }

    /// Returns the bounded query text.
    #[must_use]
    pub const fn text(&self) -> &LexicalText {
        &self.text
    }

    /// Returns the similarity mode.
    #[must_use]
    pub const fn mode(&self) -> FuzzyMode {
        self.mode
    }

    /// Returns the similarity threshold.
    #[must_use]
    pub const fn threshold(&self) -> FuzzyThreshold {
        self.threshold
    }
}

fn require_keys(object: &Map<String, Value>, allowed: &[&str]) -> Result<()> {
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid("lexical_query", "contains an unknown field"));
    }
    Ok(())
}

fn string_field<'a>(object: &'a Map<String, Value>, field: &'static str) -> Result<&'a str> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(field, "must be a string"))
}

fn u16_field(object: &Map<String, Value>, field: &'static str) -> Result<u16> {
    let value = object
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid(field, "must be a non-negative integer"))?;
    u16::try_from(value).map_err(|_| invalid(field, "must be within 1..=16384 lexemes"))
}

fn invalid(field: &'static str, reason: &'static str) -> QueryError {
    QueryError::InvalidInput {
        field,
        reason: reason.to_owned(),
    }
}
