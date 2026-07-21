//! Typed HNSW mutation planning, publication, and lock-order contracts.
//!
//! These types describe semantic mutation intent and reader-visible state.
//! They are not page codecs, WAL records, or evidence of crash durability.

include!("graph_mutation/foundations.rs");
include!("graph_mutation/reservations.rs");
include!("graph_mutation/insert_plan.rs");
include!("graph_mutation/availability.rs");
