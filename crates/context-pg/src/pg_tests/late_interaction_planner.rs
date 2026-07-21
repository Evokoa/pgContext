#[pg_test]
fn late_interaction_explain_reports_empty_collection_noop_strategy() {
    create_late_interaction_collection("m14_late_explain_empty_collection");

    let rows = hybrid_explain_structured_rows(
        "SELECT stage,
                branch,
                strategy,
                status::text,
                estimated_candidates,
                candidate_budget
           FROM pgcontext.explain_late_interaction(
                'm14_late_explain_empty_collection',
                ARRAY['[1,0]'::vector],
                'token_vectors'
           )
          WHERE stage = 'ann_planner'",
    );

    assert_eq!(
        rows,
        vec![(
            "ann_planner".to_owned(),
            Some("multi_vector".to_owned()),
            "exact_noop".to_owned(),
            "Ready".to_owned(),
            Some(0),
            Some(1_000_000),
        )]
    );
}

#[pg_test]
fn late_interaction_explain_reports_comparison_budget_rejection() {
    Spi::run(
        "CREATE TABLE public.m14_late_explain_budget (
             id bigint PRIMARY KEY,
             token_vectors vector[] NOT NULL
         )",
    )
    .expect("late-interaction explain budget table should be created");
    Spi::run(
        "INSERT INTO public.m14_late_explain_budget (id, token_vectors)
         VALUES (10, array_fill('[1,0]'::vector, ARRAY[1000]))",
    )
    .expect("late-interaction explain budget row should be inserted");
    Spi::run(
        "SELECT pgcontext.create_collection(
            'm14_late_explain_budget',
            'public.m14_late_explain_budget'
        )",
    )
    .expect("late-interaction explain budget collection should be created");
    upsert_hybrid_points("m14_late_explain_budget", &["10"]);

    let rows = hybrid_explain_structured_rows(
        "SELECT stage,
                branch,
                strategy,
                status::text,
                estimated_candidates,
                candidate_budget
           FROM pgcontext.explain_late_interaction(
                'm14_late_explain_budget',
                array_fill('[1,0]'::vector, ARRAY[1001]),
                'token_vectors'
           )
          WHERE stage = 'ann_planner'",
    );

    assert_eq!(
        rows,
        vec![(
            "ann_planner".to_owned(),
            Some("multi_vector".to_owned()),
            "rejected".to_owned(),
            "Policy".to_owned(),
            Some(1_001_000),
            Some(1_000_000),
        )]
    );
}
