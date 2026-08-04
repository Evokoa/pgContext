//! Emits the frozen Phase 9 lexical-retrieval manifest.

#![allow(clippy::print_stdout)]

use context_test::{
    P9_INDEX_STRATEGIES, P9_LEXICAL_GATES, P9_MAX_FIELDS, P9_MAX_HEADLINE_OPTIONS_BYTES,
    P9_MAX_HEADLINE_OUTPUT_BYTES, P9_MAX_HEADLINE_POINTS, P9_MAX_HEADLINE_SOURCE_BYTES,
    P9_MAX_JSON_PATH_DEPTH, P9_MAX_QUERY_BYTES, P9_MAX_QUERY_NODES, P9_QUERY_FORMS, P9_RANKERS,
    p9_lexical_manifest_hash,
};

fn main() {
    println!("manifest_hash\t{:016x}", p9_lexical_manifest_hash());
    println!("max_fields\t{P9_MAX_FIELDS}");
    println!("max_json_path_depth\t{P9_MAX_JSON_PATH_DEPTH}");
    println!("max_query_bytes\t{P9_MAX_QUERY_BYTES}");
    println!("max_query_nodes\t{P9_MAX_QUERY_NODES}");
    println!("max_headline_points\t{P9_MAX_HEADLINE_POINTS}");
    println!("max_headline_source_bytes\t{P9_MAX_HEADLINE_SOURCE_BYTES}");
    println!("max_headline_output_bytes\t{P9_MAX_HEADLINE_OUTPUT_BYTES}");
    println!("max_headline_options_bytes\t{P9_MAX_HEADLINE_OPTIONS_BYTES}");
    println!("query_forms\t{}", P9_QUERY_FORMS.join(","));
    println!("rankers\t{}", P9_RANKERS.join(","));
    println!("index_strategies\t{}", P9_INDEX_STRATEGIES.join(","));
    for gate in P9_LEXICAL_GATES {
        println!(
            "gate\trows={} queries={} top_k={} recall={} ndcg={} mrr={} p95_micros={} candidates={}",
            gate.rows,
            gate.queries,
            gate.top_k,
            gate.minimum_recall,
            gate.minimum_ndcg,
            gate.minimum_mrr,
            gate.maximum_p95_micros,
            gate.maximum_candidates,
        );
    }
}
