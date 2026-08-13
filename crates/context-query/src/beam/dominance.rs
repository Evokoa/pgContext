//! Deterministic dominance lookup with an allocation-light small-set path.

use std::collections::BTreeMap;
use std::collections::TryReserveError;

use context_core::{OccurrenceId, PointId};

use super::{AuthorizationContextToken, BeamStateId, PathPatternState};

const SMALL_DOMINANCE_CAPACITY: usize = 32;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct DominanceKey {
    occurrence_id: OccurrenceId,
    path_pattern_state: PathPatternState,
    authorization: AuthorizationContextToken,
}

#[derive(Debug)]
pub(super) struct OccurrenceIndex {
    small: Vec<(OccurrenceId, PointId)>,
    tree: Option<BTreeMap<OccurrenceId, PointId>>,
}

impl OccurrenceIndex {
    pub(super) const fn new() -> Self {
        Self {
            small: Vec::new(),
            tree: None,
        }
    }

    pub(super) fn reserve_small(&mut self, count: usize) -> Result<(), TryReserveError> {
        if count <= SMALL_DOMINANCE_CAPACITY {
            self.small.try_reserve_exact(count)?;
        }
        Ok(())
    }

    pub(super) fn len(&self) -> usize {
        self.tree.as_ref().map_or(self.small.len(), BTreeMap::len)
    }

    pub(super) fn get(&self, occurrence_id: OccurrenceId) -> Option<PointId> {
        self.tree.as_ref().map_or_else(
            || {
                self.small.iter().find_map(|(candidate, point_id)| {
                    (*candidate == occurrence_id).then_some(*point_id)
                })
            },
            |tree| tree.get(&occurrence_id).copied(),
        )
    }

    pub(super) fn insert_new(&mut self, occurrence_id: OccurrenceId, point_id: PointId) {
        if let Some(tree) = &mut self.tree {
            tree.insert(occurrence_id, point_id);
        } else if self.small.len() < SMALL_DOMINANCE_CAPACITY {
            self.small.push((occurrence_id, point_id));
        } else {
            let mut tree = BTreeMap::new();
            for (candidate, existing) in self.small.drain(..) {
                tree.insert(candidate, existing);
            }
            tree.insert(occurrence_id, point_id);
            self.tree = Some(tree);
        }
    }
}

impl DominanceKey {
    pub(super) const fn new(
        occurrence_id: OccurrenceId,
        path_pattern_state: PathPatternState,
        authorization: AuthorizationContextToken,
    ) -> Self {
        Self {
            occurrence_id,
            path_pattern_state,
            authorization,
        }
    }
}

#[derive(Debug)]
pub(super) struct DominanceIndex {
    small: Vec<(DominanceKey, BeamStateId)>,
    tree: Option<BTreeMap<DominanceKey, BeamStateId>>,
}

impl DominanceIndex {
    pub(super) const fn new() -> Self {
        Self {
            small: Vec::new(),
            tree: None,
        }
    }

    pub(super) fn reserve_small(&mut self, count: usize) -> Result<(), TryReserveError> {
        if count <= SMALL_DOMINANCE_CAPACITY {
            self.small.try_reserve_exact(count)?;
        }
        Ok(())
    }

    pub(super) fn len(&self) -> usize {
        self.tree.as_ref().map_or(self.small.len(), BTreeMap::len)
    }

    pub(super) fn get(&self, key: &DominanceKey) -> Option<BeamStateId> {
        self.tree.as_ref().map_or_else(
            || {
                self.small
                    .iter()
                    .find_map(|(candidate, state_id)| (*candidate == *key).then_some(*state_id))
            },
            |tree| tree.get(key).copied(),
        )
    }

    pub(super) fn insert_known(
        &mut self,
        key: DominanceKey,
        state_id: BeamStateId,
        replaces_existing: bool,
    ) -> bool {
        if let Some(tree) = &mut self.tree {
            tree.insert(key, state_id);
            return true;
        }
        if replaces_existing {
            if let Some((_, existing)) = self
                .small
                .iter_mut()
                .find(|(candidate, _)| *candidate == key)
            {
                *existing = state_id;
                return true;
            }
            return false;
        }
        if self.small.len() < SMALL_DOMINANCE_CAPACITY {
            self.small.push((key, state_id));
            return true;
        }
        let mut tree = BTreeMap::new();
        for (candidate, existing) in self.small.drain(..) {
            tree.insert(candidate, existing);
        }
        tree.insert(key, state_id);
        self.tree = Some(tree);
        true
    }

    pub(super) fn extend_values(&self, output: &mut Vec<BeamStateId>) {
        if let Some(tree) = &self.tree {
            output.extend(tree.values().copied());
        } else {
            output.extend(self.small.iter().map(|(_, state_id)| *state_id));
        }
    }
}
