//! PostgreSQL authorization and lifecycle boundary for detached semantic reranking.

#![allow(
    unsafe_code,
    reason = "bounded PostgreSQL datum admission must inspect raw TOAST sizes before pgrx conversion"
)]

use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    mem::size_of,
};

use context_core::{OccurrenceId, PointId, SourceVersion};
use context_filter::{Filter, parse_filter_json};
use context_query::{
    DEFAULT_QUERY_MEMORY_BYTES, MAX_RERANK_CANDIDATES, MAX_RERANK_METADATA_BYTES,
    MAX_RERANK_METADATA_ENTRIES, MAX_RERANK_REQUEST_BYTES, MAX_RERANK_TEXT_BYTES,
    MAX_RERANK_WIRE_BYTES, RERANK_CONTENT_DIGEST_BYTES, RERANK_ENVELOPE_VERSION, RerankCandidate,
    RerankContentDigest, RerankContribution, RerankMetadata, RerankModelName, RerankQuery,
    RerankRejection, RerankRequest, RerankRequestId, RerankResponse, RerankResponseCompletion,
    RerankResponsePolicy, RerankScore, validate_rerank_response_with_policy,
};
use pgrx::{JsonB, datum::DatumWithOid, prelude::*};
use serde::Deserialize;
use serde_json::{Map, Number, Value, json};
use sha2::{Digest, Sha256};

use crate::{
    error::{raise_query_error, raise_sql_error},
    lexical_catalog::{quote_literal, require_collection_owner_id},
    table_search::{
        FilterField, push_filter_parameter_args, quote_identifier, quote_qualified_identifier,
        resolve_typed_filter_plan,
    },
};

const MAX_RERANK_TTL_MILLIS: i64 = 60_000;
const MAX_RERANK_SOURCE_NAME_BYTES: usize = 128;
const RERANK_CONTENT_HASH_RULE: &str = "sha256_utf8_v1";

// These concern-oriented fragments share one private module so PostgreSQL
// datum types, preparation state, and finalization state are not exposed across
// the crate merely to keep each trust boundary reviewable.
include!("semantic_rerank/datum.rs");
include!("semantic_rerank/api.rs");
include!("semantic_rerank/preparation.rs");
include!("semantic_rerank/finalization.rs");
const MAX_RERANK_JSON_NODES: usize = 100_000;
const MAX_RERANK_JSON_DEPTH: usize = 64;
const MAX_FILTER_JSON_RAW_BYTES: usize = 256 * 1024;
const MAX_FILTER_JSON_NODES: usize = 1_024;
const MAX_RESPONSE_JSON_RAW_BYTES: usize = 256 * 1024;
const MAX_RESPONSE_JSON_NODES: usize = 4_096;
