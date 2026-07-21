//! Authoritative SQL object lifecycle metadata for the first release surface.

/// Compatibility lifecycle for a SQL-visible object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum SqlLifecycle {
    Stable,
    Experimental,
    Internal,
}

/// SQL object kind tracked by the release contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum SqlObjectKind {
    AccessMethod,
    Aggregate,
    Cast,
    Function,
    Operator,
    OperatorClass,
    Schema,
    Table,
    Trigger,
    Type,
    View,
}

/// One SQL object in the public contract registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct SqlContractObject {
    pub(crate) kind: SqlObjectKind,
    pub(crate) schema: Option<&'static str>,
    pub(crate) name: &'static str,
    pub(crate) identity: Option<&'static str>,
    pub(crate) lifecycle: SqlLifecycle,
}

impl SqlContractObject {
    const fn function(name: &'static str, identity: &'static str, lifecycle: SqlLifecycle) -> Self {
        Self {
            kind: SqlObjectKind::Function,
            schema: Some("pgcontext"),
            name,
            identity: Some(identity),
            lifecycle,
        }
    }

    const fn pgcontext_object(
        kind: SqlObjectKind,
        name: &'static str,
        lifecycle: SqlLifecycle,
    ) -> Self {
        Self::object(kind, Some("pgcontext"), name, lifecycle)
    }

    const fn stable_pgcontext_type(name: &'static str) -> Self {
        Self::pgcontext_object(SqlObjectKind::Type, name, SqlLifecycle::Stable)
    }

    const fn pgcontext_aggregate(name: &'static str, lifecycle: SqlLifecycle) -> Self {
        Self::pgcontext_object(SqlObjectKind::Aggregate, name, lifecycle)
    }

    const fn pgcontext_operator(
        name: &'static str,
        identity: &'static str,
        lifecycle: SqlLifecycle,
    ) -> Self {
        Self::object_with_identity(
            SqlObjectKind::Operator,
            Some("pgcontext"),
            name,
            identity,
            lifecycle,
        )
    }

    const fn pgcontext_operator_class(
        name: &'static str,
        identity: &'static str,
        lifecycle: SqlLifecycle,
    ) -> Self {
        Self::object_with_identity(
            SqlObjectKind::OperatorClass,
            Some("pgcontext"),
            name,
            identity,
            lifecycle,
        )
    }

    const fn object(
        kind: SqlObjectKind,
        schema: Option<&'static str>,
        name: &'static str,
        lifecycle: SqlLifecycle,
    ) -> Self {
        Self {
            kind,
            schema,
            name,
            identity: None,
            lifecycle,
        }
    }

    const fn object_with_identity(
        kind: SqlObjectKind,
        schema: Option<&'static str>,
        name: &'static str,
        identity: &'static str,
        lifecycle: SqlLifecycle,
    ) -> Self {
        Self {
            kind,
            schema,
            name,
            identity: Some(identity),
            lifecycle,
        }
    }
}

mod contract_catalog_objects;
mod contract_objects;
pub(crate) use contract_objects::SQL_CONTRACT_OBJECTS;

#[cfg(test)]
mod tests {
    use super::{SQL_CONTRACT_OBJECTS, SqlLifecycle, SqlObjectKind};
    use std::collections::BTreeSet;

    #[test]
    fn contract_entries_are_unique() {
        let mut seen = BTreeSet::new();
        for object in SQL_CONTRACT_OBJECTS {
            assert!(
                seen.insert((object.kind, object.schema, object.name, object.identity)),
                "duplicate SQL contract object: {object:?}"
            );
        }
    }

    #[test]
    fn contract_classifies_all_lifecycle_levels() {
        let lifecycles = SQL_CONTRACT_OBJECTS
            .iter()
            .map(|object| object.lifecycle)
            .collect::<BTreeSet<_>>();

        assert!(lifecycles.contains(&SqlLifecycle::Stable));
        assert!(lifecycles.contains(&SqlLifecycle::Experimental));
        assert!(lifecycles.contains(&SqlLifecycle::Internal));
    }

    #[test]
    fn stable_status_enum_types_use_generated_catalog_names() {
        let stable_types = SQL_CONTRACT_OBJECTS
            .iter()
            .filter(|object| {
                object.kind == SqlObjectKind::Type && object.lifecycle == SqlLifecycle::Stable
            })
            .map(|object| object.name)
            .collect::<BTreeSet<_>>();
        let expected = BTreeSet::from([
            "embeddingmigrationstatus",
            "indexadvisorrecommendation",
            "indexdiagnosticstatus",
            "indexlifecyclestatus",
            "indexmemoryestimatestatus",
            "optimizationstatus",
            "querycohortstatus",
            "queryexplainstatus",
            "querylatencybucket",
            "querylifecyclestate",
            "recallcheckstatus",
            "telemetrystatus",
            "vacuumadvicestatus",
        ]);

        assert!(expected.is_subset(&stable_types));
    }

    #[test]
    fn stable_search_and_query_have_distinct_contracts() {
        let stable_functions = SQL_CONTRACT_OBJECTS
            .iter()
            .filter(|object| {
                object.kind == SqlObjectKind::Function && object.lifecycle == SqlLifecycle::Stable
            })
            .collect::<Vec<_>>();

        assert!(stable_functions.iter().any(|object| {
            object.name == "search"
                && object.identity == Some("collection text, vector vector, \"limit\" integer")
        }));
        assert!(stable_functions.iter().any(|object| {
            object.name == "search"
                && object.identity
                    == Some("collection text, vector vector, filter text, \"limit\" integer")
        }));
        assert!(stable_functions.iter().any(|object| {
            object.name == "query"
                && object.identity
                    == Some(
                        "collection text, vector vector, text_query text, text_column text, \"limit\" integer"
                )
        }));
    }

    #[test]
    fn variant_sql_surfaces_remain_experimental_until_ann_promotion() {
        let expected_experimental = [
            (SqlObjectKind::Type, None, "halfvec", None),
            (SqlObjectKind::Type, None, "sparsevec", None),
            (SqlObjectKind::Type, None, "bitvec", None),
            (
                SqlObjectKind::OperatorClass,
                Some("pgcontext"),
                "halfvec_ops",
                Some("btree, halfvec"),
            ),
            (
                SqlObjectKind::OperatorClass,
                Some("pgcontext"),
                "sparsevec_ops",
                Some("btree, sparsevec"),
            ),
            (
                SqlObjectKind::OperatorClass,
                Some("pgcontext"),
                "bitvec_ops",
                Some("btree, bitvec"),
            ),
            (
                SqlObjectKind::Aggregate,
                Some("pgcontext"),
                "sum(halfvec)",
                None,
            ),
            (
                SqlObjectKind::Aggregate,
                Some("pgcontext"),
                "avg(halfvec)",
                None,
            ),
            (
                SqlObjectKind::Aggregate,
                Some("pgcontext"),
                "sum(sparsevec)",
                None,
            ),
            (
                SqlObjectKind::Aggregate,
                Some("pgcontext"),
                "avg(sparsevec)",
                None,
            ),
            (
                SqlObjectKind::Aggregate,
                Some("pgcontext"),
                "bit_or(bitvec)",
                None,
            ),
            (
                SqlObjectKind::Aggregate,
                Some("pgcontext"),
                "bit_and(bitvec)",
                None,
            ),
            (SqlObjectKind::Cast, None, "real[] AS halfvec", None),
            (SqlObjectKind::Cast, None, "integer[] AS halfvec", None),
            (
                SqlObjectKind::Cast,
                None,
                "double precision[] AS halfvec",
                None,
            ),
            (SqlObjectKind::Cast, None, "halfvec AS real[]", None),
            (SqlObjectKind::Cast, None, "bit AS bitvec", None),
            (SqlObjectKind::Cast, None, "bit varying AS bitvec", None),
            (SqlObjectKind::Cast, None, "bitvec AS bit", None),
            (SqlObjectKind::Cast, None, "bitvec AS bit varying", None),
            (SqlObjectKind::Cast, None, "boolean[] AS bitvec", None),
            (SqlObjectKind::Cast, None, "bitvec AS boolean[]", None),
            (SqlObjectKind::Cast, None, "real[] AS sparsevec", None),
            (SqlObjectKind::Cast, None, "sparsevec AS real[]", None),
            (SqlObjectKind::Cast, None, "vector AS sparsevec", None),
            (SqlObjectKind::Cast, None, "sparsevec AS vector", None),
            (
                SqlObjectKind::Function,
                Some("pgcontext"),
                "halfvec",
                Some("input text"),
            ),
            (
                SqlObjectKind::Function,
                Some("pgcontext"),
                "halfvec_dims",
                Some("vector halfvec"),
            ),
            (
                SqlObjectKind::Function,
                Some("pgcontext"),
                "halfvec_l2_distance",
                Some("\"left\" halfvec, \"right\" halfvec"),
            ),
            (
                SqlObjectKind::Function,
                Some("pgcontext"),
                "halfvec_cosine_distance",
                Some("\"left\" halfvec, \"right\" halfvec"),
            ),
            (
                SqlObjectKind::Function,
                Some("pgcontext"),
                "sparsevec",
                Some("input text"),
            ),
            (
                SqlObjectKind::Function,
                Some("pgcontext"),
                "sparsevec_dims",
                Some("vector sparsevec"),
            ),
            (
                SqlObjectKind::Function,
                Some("pgcontext"),
                "sparsevec_from_arrays",
                Some("indices integer[], \"values\" real[], dimensions integer"),
            ),
            (
                SqlObjectKind::Function,
                Some("pgcontext"),
                "sparsevec_cosine_distance",
                Some("\"left\" sparsevec, \"right\" sparsevec"),
            ),
            (
                SqlObjectKind::Function,
                Some("pgcontext"),
                "search_sparse",
                Some(
                    "query sparsevec, point_ids bigint[], vectors sparsevec[], metric text, \"limit\" integer",
                ),
            ),
            (
                SqlObjectKind::Function,
                Some("pgcontext"),
                "search_sparse",
                Some("collection text, vector_name text, query sparsevec, \"limit\" integer"),
            ),
            (
                SqlObjectKind::Function,
                Some("pgcontext"),
                "bitvec",
                Some("input text"),
            ),
            (
                SqlObjectKind::Function,
                Some("pgcontext"),
                "bitvec_dims",
                Some("vector bitvec"),
            ),
            (
                SqlObjectKind::Function,
                Some("pgcontext"),
                "bitvec_hamming_distance",
                Some("\"left\" bitvec, \"right\" bitvec"),
            ),
            (
                SqlObjectKind::Function,
                Some("pgcontext"),
                "bitvec_jaccard_distance",
                Some("\"left\" bitvec, \"right\" bitvec"),
            ),
            (
                SqlObjectKind::Function,
                Some("pgcontext"),
                "hamming_distance",
                Some("\"left\" bit, \"right\" bit"),
            ),
            (
                SqlObjectKind::Function,
                Some("pgcontext"),
                "hamming_distance",
                Some("\"left\" bit varying, \"right\" bit varying"),
            ),
            (
                SqlObjectKind::Function,
                Some("pgcontext"),
                "jaccard_distance",
                Some("\"left\" bit, \"right\" bit"),
            ),
            (
                SqlObjectKind::Function,
                Some("pgcontext"),
                "jaccard_distance",
                Some("\"left\" bit varying, \"right\" bit varying"),
            ),
        ];

        for (kind, schema, name, identity) in expected_experimental {
            let lifecycle = SQL_CONTRACT_OBJECTS
                .iter()
                .find(|object| {
                    object.kind == kind
                        && object.schema == schema
                        && object.name == name
                        && object.identity == identity
                })
                .map(|object| object.lifecycle);

            assert_eq!(
                lifecycle,
                Some(SqlLifecycle::Experimental),
                "variant SQL surface must remain experimental until ANN/index promotion is complete: {kind:?} {name} {identity:?}"
            );
        }
    }

    #[test]
    fn api_reference_documents_stable_function_signatures() {
        let api_reference =
            normalize_signature_text(include_str!("../../../docs/user_guide/api_reference.md"));
        let missing = SQL_CONTRACT_OBJECTS
            .iter()
            .filter(|object| {
                object.kind == SqlObjectKind::Function && object.lifecycle == SqlLifecycle::Stable
            })
            .map(|object| {
                let identity = object.identity.unwrap_or_default();
                normalize_signature_text(&format!("pgcontext.{}({identity})", object.name))
            })
            .filter(|signature| !api_reference.contains(signature))
            .collect::<Vec<_>>();

        assert!(
            missing.is_empty(),
            "stable SQL functions missing from API reference: {missing:?}"
        );
    }

    fn normalize_signature_text(value: &str) -> String {
        value.replace('"', "").to_ascii_lowercase()
    }
}
