//! Typestate boundary for the PostgreSQL Generic-WAL adapter.
//!
//! PostgreSQL permits fallible work between `GenericXLogStart` and
//! `GenericXLogFinish`; the actual PostgreSQL critical section exists only
//! inside `GenericXLogFinish`. This protocol therefore keeps unit validation,
//! diagnostic selection, fixed page freezing, shadow-page staging checks, and
//! every injected failure before it can yield [`HnswWalFinishPermit`]. The
//! permit is linear and deliberately exposes no callback, iterator, `Result`,
//! formatting, allocation, or page mutation API. The single-page physical
//! adapter receives an opaque token from registration, seals it only after
//! mutation, and finishes the exact stored WAL state inside this module.

use core::fmt;

use pgrx::pg_sys;

use super::{
    HnswWalMechanism, HnswWalPageAction, HnswWalUnit, HnswWalUnitKind, MAX_HNSW_WAL_PAGES,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::hnsw_am) enum HnswWalPreparationStage {
    OwnershipTransfer,
    UnitValidation,
    DiagnosticPreparation,
    PageFreeze,
    StagingSeal,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::hnsw_am) enum HnswWalPreparationFailpoint {
    OwnershipTransfer,
    UnitValidation,
    DiagnosticPreparation,
    PageFreeze,
    StagingSeal,
}

#[cfg(test)]
impl HnswWalPreparationFailpoint {
    pub(in crate::hnsw_am) const PREPARE: [Self; 4] = [
        Self::OwnershipTransfer,
        Self::UnitValidation,
        Self::DiagnosticPreparation,
        Self::PageFreeze,
    ];

    pub(in crate::hnsw_am) const fn stage(self) -> HnswWalPreparationStage {
        match self {
            Self::OwnershipTransfer => HnswWalPreparationStage::OwnershipTransfer,
            Self::UnitValidation => HnswWalPreparationStage::UnitValidation,
            Self::DiagnosticPreparation => HnswWalPreparationStage::DiagnosticPreparation,
            Self::PageFreeze => HnswWalPreparationStage::PageFreeze,
            Self::StagingSeal => HnswWalPreparationStage::StagingSeal,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::hnsw_am) enum HnswWalCriticalError {
    InjectedPreparationFailure(HnswWalPreparationStage),
    InvalidPreparedUnit { reason: &'static str },
}

impl fmt::Display for HnswWalCriticalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InjectedPreparationFailure(stage) => {
                write!(formatter, "injected WAL preparation failure at {stage:?}")
            }
            Self::InvalidPreparedUnit { reason } => {
                write!(formatter, "invalid prepared WAL unit: {reason}")
            }
        }
    }
}

impl std::error::Error for HnswWalCriticalError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::hnsw_am) enum HnswWalStagingError<AdapterError> {
    Protocol(HnswWalCriticalError),
    Adapter(AdapterError),
}

pub(in crate::hnsw_am) struct HnswWalCriticalPlan;

impl HnswWalCriticalPlan {
    pub(in crate::hnsw_am) fn prepare(
        unit: HnswWalUnit,
    ) -> Result<HnswWalPreparedUnit, HnswWalCriticalError> {
        Self::prepare_inner(unit, None)
    }

    #[cfg(test)]
    pub(in crate::hnsw_am) fn prepare_with_failpoint(
        unit: HnswWalUnit,
        failpoint: Option<HnswWalPreparationFailpoint>,
    ) -> Result<HnswWalPreparedUnit, HnswWalCriticalError> {
        Self::prepare_inner(unit, failpoint.map(HnswWalPreparationFailpoint::stage))
    }

    fn prepare_inner(
        unit: HnswWalUnit,
        injected_stage: Option<HnswWalPreparationStage>,
    ) -> Result<HnswWalPreparedUnit, HnswWalCriticalError> {
        inject(injected_stage, HnswWalPreparationStage::OwnershipTransfer)?;

        inject(injected_stage, HnswWalPreparationStage::UnitValidation)?;
        if unit.mechanism() != HnswWalMechanism::V1 {
            return Err(HnswWalCriticalError::InvalidPreparedUnit {
                reason: "Generic WAL is the only supported v1 mechanism",
            });
        }
        let page_count = unit.pages().len();
        if !(1..=MAX_HNSW_WAL_PAGES).contains(&page_count) {
            return Err(HnswWalCriticalError::InvalidPreparedUnit {
                reason: "prepared unit must contain one to four pages",
            });
        }
        if unit
            .pages()
            .iter()
            .zip(unit.pages().iter().skip(1))
            .any(|(left, right)| left.page_id() >= right.page_id())
        {
            return Err(HnswWalCriticalError::InvalidPreparedUnit {
                reason: "prepared pages must remain in strict lock order",
            });
        }

        inject(
            injected_stage,
            HnswWalPreparationStage::DiagnosticPreparation,
        )?;
        let diagnostic = diagnostic_for(unit.kind());

        inject(injected_stage, HnswWalPreparationStage::PageFreeze)?;
        let mut pages = [HnswWalPageAction::meta_allocator(); MAX_HNSW_WAL_PAGES];
        for (destination, source) in pages.iter_mut().zip(unit.pages().iter()) {
            *destination = *source;
        }

        Ok(HnswWalPreparedUnit {
            unit,
            pages,
            page_count,
            diagnostic,
        })
    }
}

#[must_use = "prepared WAL pages must be staged or the Generic-WAL state aborted"]
pub(in crate::hnsw_am) struct HnswWalPreparedUnit {
    unit: HnswWalUnit,
    pages: [HnswWalPageAction; MAX_HNSW_WAL_PAGES],
    page_count: usize,
    diagnostic: &'static str,
}

impl HnswWalPreparedUnit {
    pub(in crate::hnsw_am) const fn page_count(&self) -> usize {
        self.page_count
    }

    pub(in crate::hnsw_am) fn pages(&self) -> &[HnswWalPageAction] {
        &self.pages[..self.page_count]
    }

    pub(in crate::hnsw_am) const fn diagnostic(&self) -> &'static str {
        self.diagnostic
    }

    pub(in crate::hnsw_am) const fn unit_kind(&self) -> HnswWalUnitKind {
        self.unit.kind()
    }

    pub(in crate::hnsw_am) fn stage_pages<AdapterError>(
        self,
        stage: impl FnMut(&HnswWalPageAction) -> Result<(), AdapterError>,
    ) -> Result<HnswWalFinishPermit, HnswWalStagingError<AdapterError>> {
        self.stage_pages_inner(stage, None)
    }

    #[cfg(test)]
    pub(in crate::hnsw_am) fn stage_pages_with_failpoint<AdapterError>(
        self,
        stage: impl FnMut(&HnswWalPageAction) -> Result<(), AdapterError>,
        failpoint: Option<HnswWalPreparationFailpoint>,
    ) -> Result<HnswWalFinishPermit, HnswWalStagingError<AdapterError>> {
        self.stage_pages_inner(stage, failpoint.map(HnswWalPreparationFailpoint::stage))
    }

    fn stage_pages_inner<AdapterError>(
        self,
        mut stage: impl FnMut(&HnswWalPageAction) -> Result<(), AdapterError>,
        injected_stage: Option<HnswWalPreparationStage>,
    ) -> Result<HnswWalFinishPermit, HnswWalStagingError<AdapterError>> {
        for page in self.pages() {
            stage(page).map_err(HnswWalStagingError::Adapter)?;
        }
        inject(injected_stage, HnswWalPreparationStage::StagingSeal)
            .map_err(HnswWalStagingError::Protocol)?;
        Ok(HnswWalFinishPermit {
            proof: HnswWalFinishProof::Semantic {
                unit: self.unit,
                diagnostic: self.diagnostic,
            },
        })
    }
}

/// Linear proof that all Rust-owned fallible work precedes GenericXLogFinish.
///
/// This type intentionally has no formatting traits or callback-taking API.
/// The physical adapter keeps it live while this module invokes PostgreSQL's
/// `GenericXLogFinish`; no adapter can call that completion primitive directly.
#[must_use = "call GenericXLogFinish before consuming this permit"]
pub(in crate::hnsw_am) struct HnswWalFinishPermit {
    proof: HnswWalFinishProof,
}

enum HnswWalFinishProof {
    Semantic {
        unit: HnswWalUnit,
        diagnostic: &'static str,
    },
}

impl HnswWalFinishPermit {
    /// Commits the already staged Generic-WAL record.
    ///
    /// # Safety
    ///
    /// `state` must be the live `GenericXLogState` that registered exactly the
    /// pages staged while producing this permit. The caller must still release
    /// every registered buffer after this function returns.
    pub(in crate::hnsw_am) unsafe fn finish_generic_wal(
        self,
        state: *mut pg_sys::GenericXLogState,
    ) -> pg_sys::XLogRecPtr {
        debug_assert!(!state.is_null());
        match self.proof {
            HnswWalFinishProof::Semantic { unit, diagnostic } => {
                let _ = (unit, diagnostic);
            }
        }
        // SAFETY: delegated to the sole completion boundary below.
        unsafe { finish_generic_wal_state(state) }
    }
}

/// Opaque ownership of one registered Generic-WAL shadow page.
#[must_use = "mutate and seal the registered page before finishing Generic WAL"]
pub(in crate::hnsw_am) struct HnswWalRegisteredSinglePage {
    state: *mut pg_sys::GenericXLogState,
    page: pg_sys::Page,
}

impl HnswWalRegisteredSinglePage {
    /// Registers exactly one buffer and binds its shadow page to `state`.
    ///
    /// # Safety
    ///
    /// `state` must be live and unregistered, and `buffer` must remain pinned
    /// and exclusively locked through [`HnswWalRegisteredFinishPermit::finish`].
    pub(in crate::hnsw_am) unsafe fn register(
        state: *mut pg_sys::GenericXLogState,
        buffer: pg_sys::Buffer,
        flags: i32,
    ) -> Self {
        debug_assert!(!state.is_null());
        // SAFETY: the caller owns the live state and locked, pinned buffer.
        let page = unsafe { pg_sys::GenericXLogRegisterBuffer(state, buffer, flags) };
        Self { state, page }
    }

    pub(in crate::hnsw_am) const fn page(&self) -> pg_sys::Page {
        self.page
    }

    /// Seals the fully mutated shadow page and transfers the exact WAL state.
    pub(in crate::hnsw_am) const fn seal(self) -> HnswWalRegisteredFinishPermit {
        HnswWalRegisteredFinishPermit { state: self.state }
    }
}

/// Linear completion permit for one physically registered shadow page.
#[must_use = "finish the registered Generic-WAL state"]
pub(in crate::hnsw_am) struct HnswWalRegisteredFinishPermit {
    state: *mut pg_sys::GenericXLogState,
}

impl HnswWalRegisteredFinishPermit {
    /// Finishes the exact Generic-WAL state captured during registration.
    ///
    /// # Safety
    ///
    /// The registered buffer must remain pinned and exclusively locked. No
    /// fallible work or further shadow-page mutation may follow sealing.
    pub(in crate::hnsw_am) unsafe fn finish(self) -> pg_sys::XLogRecPtr {
        // SAFETY: the opaque registration token captured this exact live state.
        unsafe { finish_generic_wal_state(self.state) }
    }
}

/// Contains the sole direct PostgreSQL Generic-WAL completion call.
///
/// # Safety
///
/// `state` must be live and captured by a linear staging or registration
/// permit whose registered buffers remain pinned and exclusively locked.
unsafe fn finish_generic_wal_state(state: *mut pg_sys::GenericXLogState) -> pg_sys::XLogRecPtr {
    // SAFETY: callers consume a linear permit for this exact live state.
    unsafe { pg_sys::GenericXLogFinish(state) }
}

fn inject(
    injected_stage: Option<HnswWalPreparationStage>,
    current_stage: HnswWalPreparationStage,
) -> Result<(), HnswWalCriticalError> {
    if injected_stage == Some(current_stage) {
        return Err(HnswWalCriticalError::InjectedPreparationFailure(
            current_stage,
        ));
    }
    Ok(())
}

const fn diagnostic_for(kind: HnswWalUnitKind) -> &'static str {
    match kind {
        HnswWalUnitKind::ReserveNodeId => "reserve node id",
        HnswWalUnitKind::InitializePage => "initialize page",
        HnswWalUnitKind::AppendUnpublishedNode => "append unpublished node",
        HnswWalUnitKind::WriteOutboundLayer => "write outbound layer",
        HnswWalUnitKind::ReplaceNeighborLayer => "replace neighbor layer",
        HnswWalUnitKind::MarkNodeReady => "mark node ready",
        HnswWalUnitKind::PublishRoot => "publish root",
        HnswWalUnitKind::ReleaseReservation => "release reservation",
        HnswWalUnitKind::CleanupDescriptor => "cleanup descriptor",
        HnswWalUnitKind::StoreTombstone => "store tombstone",
        HnswWalUnitKind::PublishTombstone => "publish tombstone",
    }
}
