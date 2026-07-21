//! Collection catalog value-object tests.

use context_core::{
    CollectionName, Error, QualifiedTableName, SourceKey, SqlIdentifier, VectorDimensions,
    VectorName,
};

#[test]
fn collection_name_accepts_ascii_identifier_names() {
    assert_eq!(
        CollectionName::new("_tenant_01").map(CollectionName::into_string),
        Ok("_tenant_01".to_owned())
    );
}

#[test]
fn collection_name_rejects_empty_names() {
    assert_invalid_collection_name("", "must not be empty");
}

#[test]
fn collection_name_rejects_names_that_start_with_digits() {
    assert_invalid_collection_name("1tenant", "must start with an ASCII letter or underscore");
}

#[test]
fn collection_name_rejects_names_with_unsupported_characters() {
    assert_invalid_collection_name(
        "tenant-events",
        "must contain only ASCII letters, digits, and underscores",
    );
}

#[test]
fn collection_name_rejects_names_longer_than_postgres_identifier_limit() {
    let name = "a".repeat(64);

    assert_invalid_collection_name(&name, "exceeds 63 bytes");
}

#[test]
fn vector_name_accepts_sql_identifier_names() {
    assert_eq!(
        VectorName::new("embedding_v1").map(|name| name.to_string()),
        Ok("embedding_v1".to_owned())
    );
}

#[test]
fn sql_identifier_rejects_quoted_or_dotted_names() {
    assert_eq!(
        SqlIdentifier::new("docs.embedding"),
        Err(Error::InvalidIdentifier {
            kind: "SQL identifier",
            value: "docs.embedding".to_owned(),
            reason: "must contain only ASCII letters, digits, and underscores",
        })
    );
}

#[test]
fn qualified_table_name_accepts_schema_table_form() {
    let table = QualifiedTableName::new("public.documents")
        .map(|table| (table.schema().to_string(), table.table().to_string()));

    assert_eq!(table, Ok(("public".to_owned(), "documents".to_owned())));
}

#[test]
fn qualified_table_name_rejects_unqualified_names() {
    assert_eq!(
        QualifiedTableName::new("documents"),
        Err(Error::InvalidIdentifier {
            kind: "qualified table name",
            value: "documents".to_owned(),
            reason: "must use schema.table form",
        })
    );
}

#[test]
fn vector_dimensions_rejects_zero_and_values_above_policy_limit() {
    assert_eq!(
        VectorDimensions::new(0),
        Err(Error::InvalidVectorDimensions(0))
    );
    assert_eq!(
        VectorDimensions::new(16_001),
        Err(Error::InvalidVectorDimensions(16_001))
    );
    assert_eq!(
        VectorDimensions::new(1536).map(VectorDimensions::get),
        Ok(1536)
    );
}

#[test]
fn source_key_accepts_non_empty_keys() {
    assert_eq!(
        SourceKey::new("tenant-1/doc-42").map(SourceKey::into_string),
        Ok("tenant-1/doc-42".to_owned())
    );
}

#[test]
fn source_key_rejects_empty_and_oversized_keys() {
    assert_eq!(
        SourceKey::new(""),
        Err(Error::InvalidSourceKey(String::new()))
    );

    let oversized = "x".repeat(1025);
    assert_eq!(
        SourceKey::new(oversized.clone()),
        Err(Error::InvalidSourceKey(oversized))
    );
}

fn assert_invalid_collection_name(name: &str, expected_reason: &'static str) {
    assert_eq!(
        CollectionName::new(name),
        Err(Error::InvalidIdentifier {
            kind: "collection name",
            value: name.to_owned(),
            reason: expected_reason,
        })
    );
}
