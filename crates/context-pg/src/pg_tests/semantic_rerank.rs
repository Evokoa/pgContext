fn semantic_rerank_fixture(collection: &str) {
    Spi::run(&format!(
        "CREATE TABLE public.{collection} (
             id bigint PRIMARY KEY,
             tenant text NOT NULL,
             kind text NOT NULL DEFAULT 'article',
             body text NOT NULL,
             source_version bigint NOT NULL DEFAULT 1
         );
         INSERT INTO public.{collection} (id, tenant, body) VALUES
             (1, 'red', 'postgres storage internals'),
             (2, 'red', 'rust extension development'),
             (3, 'blue', 'unrelated document');
         SELECT pgcontext.create_collection('{collection}', 'public.{collection}');
         SELECT pgcontext.register_filter_column('{collection}', 'tenant', 'tenant');
         SELECT pgcontext.register_filter_column('{collection}', 'kind', 'kind');
         SELECT pgcontext.backfill_points('{collection}', 100);
         SELECT pgcontext.register_semantic_rerank_source(
             '{collection}', 'body', 'body', 'source_version'
         );"
    ))
    .expect("semantic rerank fixture should be created");
}

fn semantic_point_id(collection: &str, source_key: i64) -> i64 {
    Spi::get_one::<i64>(&format!(
        "SELECT point_id
           FROM pgcontext._visible_collection_points
          WHERE collection_id = (
                    SELECT collection_id
                      FROM pgcontext._visible_collections
                     WHERE collection_name = '{collection}'
                )
            AND source_key = '{source_key}'"
    ))
    .expect("point lookup should succeed")
    .expect("point should exist")
}

fn semantic_candidates(collection: &str) -> JsonB {
    let first = semantic_point_id(collection, 1);
    let second = semantic_point_id(collection, 2);
    JsonB(json!([
        {
            "occurrence_id": 11,
            "point_id": first,
            "fused_rank": 1,
            "fused_score": 0.8,
            "contributions": [{
                "profile": "legacy",
                "rank": 1,
                "native_score": 0.1,
                "weight": 1.0,
                "contribution": 0.01
            }],
            "metadata": [{"key": "kind", "value": "article"}]
        },
        {
            "occurrence_id": 12,
            "point_id": second,
            "fused_rank": 2,
            "fused_score": 0.7,
            "contributions": [{
                "profile": "legacy",
                "rank": 2,
                "native_score": 0.2,
                "weight": 1.0,
                "contribution": 0.009
            }]
        }
    ]))
}

fn prepare_semantic_fixture(collection: &str, policy: &str, allow_partial: bool) -> JsonB {
    Spi::get_one_with_args::<JsonB>(
        "SELECT pgcontext.prepare_semantic_rerank(
             $1, 'body', 'postgres internals', $2, 'linear-pair-v1', 7,
             5000, $3, NULL, $4
         )",
        &[
            collection.into(),
            semantic_candidates(collection).into(),
            policy.into(),
            allow_partial.into(),
        ],
    )
    .expect("semantic rerank preparation should succeed")
    .expect("semantic rerank envelope should be returned")
}

include!("semantic_rerank/preparation.rs");
include!("semantic_rerank/registration.rs");
include!("semantic_rerank/authority.rs");
include!("semantic_rerank/finalization.rs");
