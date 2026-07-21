//! Framework-free catalog vocabulary shared by pgContext adapters.

use core::fmt;

use crate::policy::{
    MAX_COLLECTION_NAME_BYTES, MAX_SOURCE_KEY_BYTES, MAX_SQL_IDENTIFIER_BYTES,
    MAX_VECTOR_DIMENSIONS,
};
use crate::{Error, Result};

/// Validated SQL-visible pgContext collection name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CollectionName(String);

impl CollectionName {
    /// Validates and stores a pgContext collection name.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidIdentifier`] when the name is empty, too long,
    /// starts with an unsupported character, or contains unsupported
    /// characters. Collection names are restricted to ASCII identifiers so
    /// later SQL generation can distinguish user names from SQL identifiers.
    pub fn new(name: impl Into<String>) -> Result<Self> {
        let name = name.into();
        validate_collection_name(&name)?;
        Ok(Self(name))
    }

    /// Returns the validated collection name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the value and returns the stored name.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for CollectionName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Validated pgContext vector registration name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VectorName(String);

impl VectorName {
    /// Validates and stores a named vector registration.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidIdentifier`] when the vector name is not a
    /// PostgreSQL-compatible ASCII identifier.
    pub fn new(name: impl Into<String>) -> Result<Self> {
        let name = name.into();
        validate_identifier(&name, "vector name", MAX_SQL_IDENTIFIER_BYTES)?;
        Ok(Self(name))
    }

    /// Returns the validated vector name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for VectorName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Validated unquoted SQL identifier.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SqlIdentifier(String);

impl SqlIdentifier {
    /// Validates and stores an unquoted SQL identifier.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidIdentifier`] when the identifier is empty, too
    /// long, starts with an unsupported character, or contains unsupported
    /// characters.
    pub fn new(identifier: impl Into<String>) -> Result<Self> {
        let identifier = identifier.into();
        validate_identifier(&identifier, "SQL identifier", MAX_SQL_IDENTIFIER_BYTES)?;
        Ok(Self(identifier))
    }

    /// Returns the validated identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SqlIdentifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Validated schema-qualified table name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualifiedTableName {
    schema: SqlIdentifier,
    table: SqlIdentifier,
}

impl QualifiedTableName {
    /// Parses and validates a `schema.table` SQL table name.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidIdentifier`] when the table name is not exactly
    /// two dot-separated SQL identifiers.
    pub fn new(name: impl AsRef<str>) -> Result<Self> {
        let name = name.as_ref();
        let Some((schema, table)) = name.split_once('.') else {
            return Err(invalid_identifier(
                "qualified table name",
                name,
                "must use schema.table form",
            ));
        };
        if table.contains('.') {
            return Err(invalid_identifier(
                "qualified table name",
                name,
                "must use schema.table form",
            ));
        }

        Ok(Self {
            schema: SqlIdentifier::new(schema)?,
            table: SqlIdentifier::new(table)?,
        })
    }

    /// Returns the table schema.
    #[must_use]
    pub fn schema(&self) -> &SqlIdentifier {
        &self.schema
    }

    /// Returns the table name.
    #[must_use]
    pub fn table(&self) -> &SqlIdentifier {
        &self.table
    }

    /// Returns the validated `schema.table` name.
    #[must_use]
    pub fn as_qualified_name(&self) -> String {
        format!("{}.{}", self.schema, self.table)
    }
}

/// Validated vector dimension count.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VectorDimensions(usize);

impl VectorDimensions {
    /// Validates a vector dimension count.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidVectorDimensions`] when the count is zero or
    /// above pgContext's current vector policy limit.
    pub fn new(dimensions: usize) -> Result<Self> {
        if dimensions == 0 || dimensions > MAX_VECTOR_DIMENSIONS {
            return Err(Error::InvalidVectorDimensions(dimensions));
        }
        Ok(Self(dimensions))
    }

    /// Returns the validated dimension count.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// Validated source-table row key used in point mappings.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceKey(String);

impl SourceKey {
    /// Validates and stores an application-provided source row key.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidSourceKey`] when the key is empty or exceeds the
    /// current catalog storage policy.
    pub fn new(key: impl Into<String>) -> Result<Self> {
        let key = key.into();
        if key.is_empty() || key.len() > MAX_SOURCE_KEY_BYTES {
            return Err(Error::InvalidSourceKey(key));
        }
        Ok(Self(key))
    }

    /// Returns the validated source key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the value and returns the stored source key.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for SourceKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

fn validate_collection_name(name: &str) -> Result<()> {
    validate_identifier(name, "collection name", MAX_COLLECTION_NAME_BYTES)
}

fn validate_identifier(name: &str, kind: &'static str, max_bytes: usize) -> Result<()> {
    if name.is_empty() {
        return Err(invalid_identifier(kind, name, "must not be empty"));
    }
    if name.len() > max_bytes {
        return Err(invalid_identifier(kind, name, "exceeds 63 bytes"));
    }
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return Err(invalid_identifier(kind, name, "must not be empty"));
    };

    if !is_identifier_start(first) {
        return Err(invalid_identifier(
            kind,
            name,
            "must start with an ASCII letter or underscore",
        ));
    }

    if chars.any(|character| !is_identifier_continue(character)) {
        return Err(invalid_identifier(
            kind,
            name,
            "must contain only ASCII letters, digits, and underscores",
        ));
    }

    Ok(())
}

fn is_identifier_start(character: char) -> bool {
    character == '_' || character.is_ascii_alphabetic()
}

fn is_identifier_continue(character: char) -> bool {
    character == '_' || character.is_ascii_alphanumeric()
}

fn invalid_identifier(kind: &'static str, name: &str, reason: &'static str) -> Error {
    Error::InvalidIdentifier {
        kind,
        value: name.to_owned(),
        reason,
    }
}
