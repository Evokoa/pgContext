#![allow(clippy::expect_used, clippy::panic)]

use context_index::{
    GraphAllocationState, GraphMutationId, GraphNodeReservation, GraphPageId, GraphRecordRevision,
    HnswNodeId,
};

use super::critical_section::*;
use super::*;

fn reservation() -> GraphNodeReservation {
    let mutation_id = GraphMutationId::new(71).expect("fixture mutation id must be nonzero");
    let node_id = HnswNodeId::new(4);
    let mut allocator = GraphAllocationState::new(node_id);
    allocator
        .reserve(mutation_id, node_id)
        .expect("fixture reservation should succeed")
}

fn unit() -> HnswWalUnit {
    HnswWalUnit::reserve_node(reservation())
}

fn two_page_unit() -> HnswWalUnit {
    let mutation_id = reservation().mutation_id();
    let expected_revision = GraphRecordRevision::new(9);
    let pages = HnswWalPageSet::new(&[
        HnswWalPageAction::directory(
            GraphPageId::new(11),
            HnswWalDirectoryWrite::RemoveDescriptor {
                mutation_id,
                expected_revision,
            },
        )
        .expect("cleanup directory page should be valid"),
        HnswWalPageAction::delete_descriptor_header(
            GraphPageId::new(12),
            mutation_id,
            expected_revision,
        )
        .expect("cleanup descriptor page should be valid"),
    ])
    .expect("two-page cleanup set should be valid");
    HnswWalUnit::cleanup_descriptor(mutation_id, expected_revision, pages)
        .expect("two-page cleanup unit should be valid")
}

#[test]
fn every_preparation_failpoint_returns_before_a_finish_permit_exists() {
    for failpoint in HnswWalPreparationFailpoint::PREPARE {
        let result = HnswWalCriticalPlan::prepare_with_failpoint(unit(), Some(failpoint));
        let Err(error) = result else {
            panic!("failpoint {failpoint:?} unexpectedly produced a prepared plan");
        };
        assert_eq!(
            error,
            HnswWalCriticalError::InjectedPreparationFailure(failpoint.stage())
        );
    }
}

#[test]
fn fixed_preparation_precedes_fallible_staging_and_a_terminal_finish_permit() {
    let prepared = HnswWalCriticalPlan::prepare(unit()).expect("valid unit should prepare");
    assert_eq!(prepared.page_count(), 1);
    assert_eq!(prepared.pages().len(), 1);
    assert_eq!(prepared.diagnostic(), "reserve node id");
    assert_eq!(prepared.unit_kind(), HnswWalUnitKind::ReserveNodeId);

    let mut staged = 0;
    let result = prepared.stage_pages(|_page| {
        staged += 1;
        Ok::<(), &'static str>(())
    });
    let Ok(_permit) = result else {
        panic!("every prepared shadow page should stage");
    };
    assert_eq!(staged, 1);
}

#[test]
fn any_shadow_page_staging_error_prevents_the_finish_permit() {
    for fail_at in 0..2 {
        let mut visited = Vec::new();
        let result = HnswWalCriticalPlan::prepare(two_page_unit())
            .expect("valid two-page unit should prepare")
            .stage_pages(|page| {
                let ordinal = visited.len();
                visited.push(page.page_id());
                if ordinal == fail_at {
                    Err(ordinal)
                } else {
                    Ok(())
                }
            });
        let Err(error) = result else {
            panic!("failure at page {fail_at} unexpectedly produced a finish permit");
        };
        assert_eq!(visited.len(), fail_at + 1);
        assert_eq!(error, HnswWalStagingError::Adapter(fail_at));
    }
}

#[test]
fn every_frozen_page_is_staged_once_in_lock_order_before_the_permit() {
    let mut visited = Vec::new();
    let result = HnswWalCriticalPlan::prepare(two_page_unit())
        .expect("valid two-page unit should prepare")
        .stage_pages(|page| {
            visited.push(page.page_id());
            Ok::<(), core::convert::Infallible>(())
        });
    let Ok(_permit) = result else {
        panic!("two-page staging should produce a finish permit");
    };
    assert_eq!(visited, [GraphPageId::new(11), GraphPageId::new(12)]);
}

#[test]
fn staging_failpoint_is_still_before_the_finish_permit_boundary() {
    let prepared = HnswWalCriticalPlan::prepare(unit()).expect("valid unit should prepare");
    let result = prepared.stage_pages_with_failpoint(
        |_page| Ok::<(), &'static str>(()),
        Some(HnswWalPreparationFailpoint::StagingSeal),
    );
    let Err(error) = result else {
        panic!("staging failpoint unexpectedly produced a finish permit");
    };
    assert_eq!(
        error,
        HnswWalStagingError::Protocol(HnswWalCriticalError::InjectedPreparationFailure(
            HnswWalPreparationStage::StagingSeal,
        ))
    );
}
