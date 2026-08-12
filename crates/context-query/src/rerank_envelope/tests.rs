    use super::*;

    fn occurrence(id: u64) -> OccurrenceId {
        OccurrenceId::new(id).expect("nonzero occurrence")
    }

    fn candidate(id: u64) -> RerankCandidate {
        let score_id = u32::try_from(id).expect("fixture score identity");
        RerankCandidate::new(
            occurrence(id),
            PointId::new(id),
            SourceVersion::new(1).expect("source version"),
            RerankContentDigest::new([1; RERANK_CONTENT_DIGEST_BYTES]),
            "authorized text",
            usize::try_from(id).expect("fixture rank"),
            1.0 / f64::from(score_id),
            vec![
                RerankContribution::new(
                    "fixture",
                    usize::try_from(id).expect("rank"),
                    0.1,
                    1.0,
                    0.01,
                )
                .expect("contribution"),
            ],
            Vec::new(),
        )
        .expect("candidate")
    }

    fn request() -> RerankRequest {
        RerankRequest::new(
            RerankRequestId::new(7).expect("request id"),
            RerankModelName::new("fixture-v1").expect("model"),
            3,
            1_000,
            RerankQuery::new("query").expect("query"),
            vec![candidate(1), candidate(2)],
        )
        .expect("request")
    }

    fn response(scores: Vec<RerankScore>) -> RerankResponse {
        RerankResponse::new(
            RERANK_ENVELOPE_VERSION,
            RerankRequestId::new(7).expect("request id"),
            RerankModelName::new("fixture-v1").expect("model"),
            3,
            scores,
        )
    }

    fn score(id: u64, value: f64) -> RerankScore {
        RerankScore::new(occurrence(id), value).expect("score")
    }

    #[test]
    fn identities_and_scores_reject_reserved_and_non_finite_values() {
        assert!(RerankRequestId::new(0).is_err());
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(
                RerankScore::new(occurrence(1), value).is_err(),
                "score {value} must be rejected"
            );
        }
    }

    #[test]
    fn requests_bound_candidate_count_uniqueness_and_total_bytes() {
        assert!(
            RerankRequest::new(
                RerankRequestId::new(1).expect("id"),
                RerankModelName::new("fixture").expect("model"),
                1,
                0,
                RerankQuery::new("query").expect("query"),
                Vec::new()
            )
            .is_err()
        );
        assert!(
            RerankRequest::new(
                RerankRequestId::new(1).expect("id"),
                RerankModelName::new("fixture").expect("model"),
                0,
                0,
                RerankQuery::new("query").expect("query"),
                vec![candidate(1)]
            )
            .is_err(),
            "a zero model revision must be rejected"
        );
        assert!(
            RerankRequest::new(
                RerankRequestId::new(1).expect("id"),
                RerankModelName::new("fixture").expect("model"),
                1,
                0,
                RerankQuery::new("query").expect("query"),
                vec![candidate(1), candidate(1)]
            )
            .is_err(),
            "a repeated occurrence must be rejected"
        );

        let mut repeated_point = candidate(2);
        repeated_point.point_id = PointId::new(1);
        assert!(
            RerankRequest::new(
                RerankRequestId::new(1).expect("id"),
                RerankModelName::new("fixture").expect("model"),
                1,
                0,
                RerankQuery::new("query").expect("query"),
                vec![candidate(1), repeated_point]
            )
            .is_err(),
            "a repeated point must be rejected"
        );

        let oversized = (1..=MAX_RERANK_CANDIDATES + 1)
            .map(|id| candidate(id as u64))
            .collect::<Vec<_>>();
        assert!(
            RerankRequest::new(
                RerankRequestId::new(1).expect("id"),
                RerankModelName::new("fixture").expect("model"),
                1,
                0,
                RerankQuery::new("query").expect("query"),
                oversized
            )
            .is_err()
        );

        let mut retained = String::with_capacity(MAX_RERANK_REQUEST_BYTES + 1);
        retained.push('x');
        let retained = RerankCandidate::new(
            occurrence(1),
            PointId::new(1),
            SourceVersion::new(1).expect("version"),
            RerankContentDigest::new([1; RERANK_CONTENT_DIGEST_BYTES]),
            retained,
            1,
            0.1,
            vec![RerankContribution::new("fixture", 1, 0.1, 1.0, 0.01).expect("contribution")],
            Vec::new(),
        )
        .expect("short text remains individually valid");
        assert!(
            RerankRequest::new(
                RerankRequestId::new(1).expect("id"),
                RerankModelName::new("fixture").expect("model"),
                1,
                0,
                RerankQuery::new("query").expect("query"),
                vec![retained],
            )
            .is_err(),
            "retained string capacity must count against the request budget"
        );

        let mut retained_profile = String::with_capacity(MAX_RERANK_REQUEST_BYTES + 1);
        retained_profile.push_str("fixture");
        let retained_profile = RerankCandidate::new(
            occurrence(1),
            PointId::new(1),
            SourceVersion::new(1).expect("version"),
            RerankContentDigest::new([1; RERANK_CONTENT_DIGEST_BYTES]),
            "text",
            1,
            0.1,
            vec![
                RerankContribution::new(retained_profile, 1, 0.1, 1.0, 0.01)
                    .expect("bounded contribution"),
            ],
            Vec::new(),
        )
        .expect("short contribution profile remains individually valid");
        assert!(
            RerankRequest::new(
                RerankRequestId::new(1).expect("id"),
                RerankModelName::new("fixture").expect("model"),
                1,
                0,
                RerankQuery::new("query").expect("query"),
                vec![retained_profile],
            )
            .is_err(),
            "retained contribution-profile capacity must count against the request budget"
        );
    }

    #[test]
    fn candidates_bound_text_and_allow_listed_metadata() {
        assert!(
            RerankCandidate::new(
                occurrence(1),
                PointId::new(1),
                SourceVersion::new(1).expect("version"),
                RerankContentDigest::new([1; RERANK_CONTENT_DIGEST_BYTES]),
                "x".repeat(MAX_RERANK_TEXT_BYTES + 1),
                1,
                0.1,
                vec![RerankContribution::new("fixture", 1, 0.1, 1.0, 0.01).expect("contribution")],
                Vec::new(),
            )
            .is_err()
        );
        let too_many = (0..=MAX_RERANK_METADATA_ENTRIES)
            .map(|index| RerankMetadata::new(format!("k{index}"), "v").expect("metadata"))
            .collect::<Vec<_>>();
        assert!(
            RerankCandidate::new(
                occurrence(1),
                PointId::new(1),
                SourceVersion::new(1).expect("version"),
                RerankContentDigest::new([1; RERANK_CONTENT_DIGEST_BYTES]),
                "text",
                1,
                0.1,
                vec![RerankContribution::new("fixture", 1, 0.1, 1.0, 0.01).expect("contribution")],
                too_many,
            )
            .is_err()
        );
        let duplicated = vec![
            RerankMetadata::new("tenant", "a").expect("metadata"),
            RerankMetadata::new("tenant", "b").expect("metadata"),
        ];
        assert!(
            RerankCandidate::new(
                occurrence(1),
                PointId::new(1),
                SourceVersion::new(1).expect("version"),
                RerankContentDigest::new([1; RERANK_CONTENT_DIGEST_BYTES]),
                "text",
                1,
                0.1,
                vec![RerankContribution::new("fixture", 1, 0.1, 1.0, 0.01).expect("contribution")],
                duplicated,
            )
            .is_err(),
            "a repeated metadata key must be rejected, not last-wins"
        );
        assert!(RerankMetadata::new("", "v").is_err());
    }

    #[test]
    fn a_well_formed_response_is_accepted_in_provider_order() {
        let accepted = validate_rerank_response(
            &request(),
            &response(vec![score(2, 0.9), score(1, 0.1)]),
            500,
        )
        .expect("well-formed response");
        assert_eq!(
            accepted
                .iter()
                .map(|score| score.occurrence_id())
                .collect::<Vec<_>>(),
            vec![occurrence(2), occurrence(1)]
        );
    }

    #[test]
    fn canonical_response_order_ignores_provider_array_order_and_breaks_ties_by_occurrence() {
        let request = request();
        let validated = validate_rerank_response_with_policy(
            &request,
            &response(vec![score(2, 0.5), score(1, 0.5)]),
            500,
            RerankResponsePolicy::RequireComplete,
        )
        .expect("response should validate");
        let ordered = validated.into_ordered_scores();
        assert_eq!(ordered[0].occurrence_id(), occurrence(1));
        assert_eq!(ordered[1].occurrence_id(), occurrence(2));
    }

    #[test]
    fn a_response_answering_a_different_request_is_refused() {
        let mismatched = RerankResponse::new(
            RERANK_ENVELOPE_VERSION,
            RerankRequestId::new(8).expect("id"),
            RerankModelName::new("fixture-v1").expect("model"),
            3,
            vec![score(1, 1.0)],
        );
        assert_eq!(
            validate_rerank_response(&request(), &mismatched, 0),
            Err(RerankRejection::RequestMismatch)
        );
    }

    #[test]
    fn version_model_and_expiry_mismatches_are_each_refused() {
        let wrong_version = RerankResponse::new(
            RERANK_ENVELOPE_VERSION + 1,
            RerankRequestId::new(7).expect("id"),
            RerankModelName::new("fixture-v1").expect("model"),
            3,
            vec![score(1, 1.0)],
        );
        assert_eq!(
            validate_rerank_response(&request(), &wrong_version, 0),
            Err(RerankRejection::VersionMismatch)
        );

        let wrong_model_name = RerankResponse::new(
            RERANK_ENVELOPE_VERSION,
            RerankRequestId::new(7).expect("id"),
            RerankModelName::new("wrong-model").expect("model"),
            3,
            vec![score(1, 1.0)],
        );
        assert_eq!(
            validate_rerank_response(&request(), &wrong_model_name, 0),
            Err(RerankRejection::ModelMismatch)
        );

        let wrong_model_revision = RerankResponse::new(
            RERANK_ENVELOPE_VERSION,
            RerankRequestId::new(7).expect("id"),
            RerankModelName::new("fixture-v1").expect("model"),
            4,
            vec![score(1, 1.0)],
        );
        assert_eq!(
            validate_rerank_response(&request(), &wrong_model_revision, 0),
            Err(RerankRejection::ModelRevisionMismatch)
        );

        assert_eq!(
            validate_rerank_response(&request(), &response(vec![score(1, 1.0)]), 1_001),
            Err(RerankRejection::Expired)
        );
        assert_eq!(
            validate_rerank_response(&request(), &response(vec![score(1, 1.0)]), 1_000),
            Err(RerankRejection::Expired),
            "a request is expired starting at its declared instant"
        );
    }

    #[test]
    fn injected_duplicated_and_oversized_score_sets_are_refused() {
        assert_eq!(
            validate_rerank_response(&request(), &response(vec![score(99, 1.0)]), 0),
            Err(RerankRejection::UnknownOccurrence),
            "a provider must not score an occurrence it was never given"
        );
        assert_eq!(
            validate_rerank_response(&request(), &response(vec![score(1, 1.0), score(1, 2.0)]), 0),
            Err(RerankRejection::DuplicateOccurrence)
        );
        assert_eq!(
            validate_rerank_response(
                &request(),
                &response(vec![score(1, 1.0), score(2, 1.0), score(1, 1.0)]),
                0
            ),
            Err(RerankRejection::TooManyScores)
        );
    }

    #[test]
    fn a_provider_may_return_fewer_scores_than_it_was_given() {
        let accepted = validate_rerank_response(&request(), &response(vec![score(1, 0.5)]), 0)
            .expect("a partial score set is well formed");
        assert_eq!(accepted.len(), 1);
    }

    #[test]
    fn rejections_and_policies_carry_stable_names() {
        assert_eq!(
            RerankRejection::UnknownOccurrence.stable_name(),
            "unknown_occurrence"
        );
        assert_ne!(
            RerankFallbackPolicy::Require,
            RerankFallbackPolicy::DegradeWithoutRerank
        );
    }

    #[test]
    fn query_model_digest_and_fusion_evidence_are_bounded() {
        assert!(RerankQuery::new("q".repeat(MAX_RERANK_QUERY_BYTES)).is_ok());
        assert!(RerankQuery::new("q".repeat(MAX_RERANK_QUERY_BYTES + 1)).is_err());
        assert!(RerankModelName::new("fixture-v1").is_ok());
        assert!(RerankModelName::new(" ").is_err());
        assert!(RerankModelName::new("m".repeat(MAX_RERANK_MODEL_NAME_BYTES + 1)).is_err());

        let digest = RerankContentDigest::new([0xabu8; RERANK_CONTENT_DIGEST_BYTES]);
        assert_eq!(digest.as_bytes(), &[0xabu8; RERANK_CONTENT_DIGEST_BYTES]);

        let contribution =
            RerankContribution::new("legacy_v1", 1, 0.25, 1.0, 0.01).expect("bounded contribution");
        assert_eq!(contribution.profile(), "legacy_v1");
        assert!(RerankContribution::new("legacy_v1", 0, 0.25, 1.0, 0.01).is_err());
        assert!(RerankContribution::new("legacy_v1", 1, f64::NAN, 1.0, 0.01).is_err());
        assert!(RerankContribution::new("legacy_v1", 1, 0.25, 0.0, 0.01).is_err());
    }

    #[test]
    fn rich_candidates_and_requests_account_every_released_byte() {
        let evidence =
            vec![RerankContribution::new("legacy_v1", 1, 0.25, 1.0, 0.01).expect("contribution")];
        let candidate = RerankCandidate::new(
            occurrence(1),
            PointId::new(1),
            SourceVersion::new(1).expect("version"),
            RerankContentDigest::new([1; RERANK_CONTENT_DIGEST_BYTES]),
            "authorized text",
            1,
            0.01,
            evidence,
            vec![RerankMetadata::new("language", "en").expect("metadata")],
        )
        .expect("candidate");
        assert_eq!(candidate.fused_rank(), 1);
        assert_eq!(
            candidate.content_digest().as_bytes(),
            &[1; RERANK_CONTENT_DIGEST_BYTES]
        );
        assert_eq!(candidate.contributions().len(), 1);

        let request = RerankRequest::new(
            RerankRequestId::new(9).expect("request"),
            RerankModelName::new("fixture-v1").expect("model"),
            3,
            1_000,
            RerankQuery::new("postgres retrieval").expect("query"),
            vec![candidate],
        )
        .expect("rich request");
        assert_eq!(request.query().as_str(), "postgres retrieval");
        assert_eq!(request.model().as_str(), "fixture-v1");
        assert!(request.projected_bytes() > "authorized text".len());
    }

    #[test]
    fn response_completeness_is_policy_owned_and_never_silent() {
        let request = request();
        let partial = response(vec![score(1, 0.5)]);
        let validated = validate_rerank_response_with_policy(
            &request,
            &partial,
            0,
            RerankResponsePolicy::AllowPartial,
        )
        .expect("visible partial response");
        assert_eq!(validated.completion(), RerankResponseCompletion::Partial);

        assert_eq!(
            validate_rerank_response_with_policy(
                &request,
                &partial,
                0,
                RerankResponsePolicy::RequireComplete,
            ),
            Err(RerankRejection::Incomplete)
        );
    }
