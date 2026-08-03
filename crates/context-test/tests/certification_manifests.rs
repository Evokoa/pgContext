//! Fixed certification-tier manifest contracts.

use std::{error::Error, io};

use context_test::{CertificationArtifactKind, CertificationDatasetManifest, CertificationTier};

#[test]
fn certification_tiers_are_fixed_and_retain_raw_artifacts() {
    let manifests = CertificationDatasetManifest::release_tiers();
    assert_eq!(manifests.len(), 3);
    assert_eq!(manifests[0].tier(), CertificationTier::Rows100k);
    assert_eq!(manifests[0].rows(), 100_000);
    assert_eq!(manifests[1].rows(), 1_000_000);
    assert_eq!(manifests[2].rows(), 10_000_000);
    for manifest in manifests {
        assert!(manifest.retain_raw_artifacts());
        assert!(manifest.tenant_count() > 1);
        assert!(manifest.filter_modulus() > 1);
        assert_ne!(manifest.exact_ground_truth_checksum(), 0);
        assert_eq!(manifest.generator_version(), 1);
        let artifacts = manifest.artifacts();
        assert_eq!(artifacts[0].kind(), CertificationArtifactKind::RawInputs);
        assert_eq!(
            artifacts[1].kind(),
            CertificationArtifactKind::ExactGroundTruth
        );
        for artifact in artifacts {
            assert_eq!(artifact.checksum_algorithm(), "fnv1a64-v1");
            assert!(artifact.relative_path().starts_with("certification/"));
            assert_ne!(artifact.checksum(), 0);
        }
    }
}

#[test]
fn generators_reproduce_retained_checksums_and_detect_corruption() -> Result<(), Box<dyn Error>> {
    for manifest in CertificationDatasetManifest::release_tiers() {
        let raw_checksum =
            manifest.generate_artifact(CertificationArtifactKind::RawInputs, io::sink())?;
        assert_eq!(raw_checksum, manifest.artifacts()[0].checksum());
        let mut exact = Vec::new();
        let checksum =
            manifest.generate_artifact(CertificationArtifactKind::ExactGroundTruth, &mut exact)?;
        assert_eq!(checksum, manifest.exact_ground_truth_checksum());
        manifest.verify_artifact(
            CertificationArtifactKind::ExactGroundTruth,
            exact.as_slice(),
        )?;
        exact[0] ^= 1;
        assert!(
            manifest
                .verify_artifact(
                    CertificationArtifactKind::ExactGroundTruth,
                    exact.as_slice()
                )
                .is_err()
        );
    }

    let manifest = CertificationDatasetManifest::rows_100k();
    let mut raw = Vec::new();
    let checksum = manifest.generate_artifact(CertificationArtifactKind::RawInputs, &mut raw)?;
    assert_eq!(checksum, manifest.artifacts()[0].checksum());
    manifest.verify_artifact(CertificationArtifactKind::RawInputs, raw.as_slice())?;
    Ok(())
}

#[test]
fn vector_expansion_is_counter_based_and_reproducible() {
    let manifest = CertificationDatasetManifest::rows_100k();
    assert_eq!(
        manifest.vector_component(17, 4),
        manifest.vector_component(17, 4)
    );
    assert_ne!(
        manifest.vector_component(17, 4),
        manifest.vector_component(17, 5)
    );
    assert_ne!(
        manifest.vector_component(17, 0),
        manifest.vector_component(17, 4)
    );
}

#[test]
fn exact_neighbors_match_an_independent_filtered_brute_force_oracle() {
    let manifest = CertificationDatasetManifest::rows_100k();
    let expected = manifest.exact_neighbors(7);
    assert_eq!(expected.len(), manifest.exact_k());
    assert_eq!(expected[0].squared_l2(), 0.0);

    let query_point_id = expected[0].point_id();
    let query_tenant = manifest.tenant_for_point(query_point_id);
    let query_filter = manifest.filter_for_point(query_point_id);
    let mut brute_force = (1..=manifest.rows())
        .filter(|point_id| {
            manifest.tenant_for_point(*point_id) == query_tenant
                && manifest.filter_for_point(*point_id) == query_filter
        })
        .map(|point_id| {
            let score = (0..manifest.dimensions())
                .map(|dimension| {
                    let delta = f64::from(manifest.vector_component(query_point_id, dimension))
                        - f64::from(manifest.vector_component(point_id, dimension));
                    delta * delta
                })
                .sum::<f64>();
            (point_id, score)
        })
        .collect::<Vec<_>>();
    brute_force.sort_unstable_by(|left, right| {
        left.1
            .total_cmp(&right.1)
            .then_with(|| left.0.cmp(&right.0))
    });

    for (actual, (point_id, score)) in expected.iter().zip(brute_force) {
        assert_eq!(actual.point_id(), point_id);
        assert_eq!(actual.squared_l2(), score);
    }
}

#[test]
fn controlled_filter_is_independent_and_removes_tenant_candidates() {
    let manifest = CertificationDatasetManifest::rows_100k();
    let tenant = manifest.tenant_for_point(1);
    let tenant_points = (1..=manifest.rows())
        .filter(|point_id| manifest.tenant_for_point(*point_id) == tenant)
        .collect::<Vec<_>>();
    let first_filter = manifest.filter_for_point(tenant_points[0]);
    assert!(
        tenant_points
            .iter()
            .any(|point_id| manifest.filter_for_point(*point_id) != first_filter)
    );
    let filtered_count = tenant_points
        .iter()
        .filter(|point_id| manifest.filter_for_point(**point_id) == first_filter)
        .count();
    assert!(filtered_count > 0);
    assert!(filtered_count < tenant_points.len());
}

#[test]
fn every_release_query_has_a_full_exact_top_k() {
    for manifest in CertificationDatasetManifest::release_tiers() {
        for query in 0..manifest.exact_query_count() {
            assert_eq!(manifest.exact_neighbors(query).len(), manifest.exact_k());
        }
    }
}

#[test]
fn raw_vector_seed_is_the_authority_for_vector_expansion() -> Result<(), Box<dyn Error>> {
    let manifest = CertificationDatasetManifest::rows_100k();
    let mut raw = Vec::new();
    manifest.generate_artifact(CertificationArtifactKind::RawInputs, &mut raw)?;
    let vector_seed = u64::from_le_bytes(raw[16..24].try_into()?);
    assert_eq!(vector_seed, manifest.vector_seed(1));
    assert_eq!(
        manifest.vector_component(1, 0),
        manifest.expand_vector_seed_component(vector_seed, 0)
    );
    assert_eq!(
        manifest.vector_component(1, 99),
        manifest.expand_vector_seed_component(vector_seed, 99)
    );
    Ok(())
}

#[test]
fn certification_manifests_are_reproducible() {
    assert_eq!(
        CertificationDatasetManifest::release_tiers(),
        CertificationDatasetManifest::release_tiers()
    );
}
