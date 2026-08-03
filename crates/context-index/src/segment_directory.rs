//! Pure policy for immutable HNSW segments and one bounded active delta.

use std::collections::{BTreeMap, BTreeSet};

/// Stable immutable segment identity within one PostgreSQL index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SegmentId(u64);

impl SegmentId {
    /// Returns the numeric segment identity.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// One published immutable HNSW segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HnswSegment {
    id: SegmentId,
    generation: u64,
    rows: u64,
    bytes: u64,
}

impl HnswSegment {
    /// Returns the segment identity.
    #[must_use]
    pub const fn id(self) -> SegmentId {
        self.id
    }

    /// Returns the directory generation that first published this segment.
    #[must_use]
    pub const fn generation(self) -> u64 {
        self.generation
    }

    /// Returns the number of authoritative rows represented by the segment.
    #[must_use]
    pub const fn rows(self) -> u64 {
        self.rows
    }

    /// Returns the encoded immutable payload bytes.
    #[must_use]
    pub const fn bytes(self) -> u64 {
        self.bytes
    }
}

/// Hard bounds for one segmented HNSW directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentPolicy {
    delta_max_rows: u64,
    max_segments: usize,
    compaction_fan_in: usize,
    parallel_min_segments: usize,
}

impl SegmentPolicy {
    /// Creates a bounded segment policy.
    ///
    /// # Errors
    ///
    /// Rejects zero bounds, a fan-in below two, a fan-in above the segment
    /// cap, or a parallel threshold above the cap.
    pub const fn new(
        delta_max_rows: u64,
        max_segments: usize,
        compaction_fan_in: usize,
        parallel_min_segments: usize,
    ) -> Result<Self, SegmentDirectoryError> {
        if delta_max_rows == 0 {
            return Err(SegmentDirectoryError::InvalidPolicy("delta_max_rows"));
        }
        if max_segments < 2 {
            return Err(SegmentDirectoryError::InvalidPolicy("max_segments"));
        }
        if compaction_fan_in < 2 || compaction_fan_in > max_segments {
            return Err(SegmentDirectoryError::InvalidPolicy("compaction_fan_in"));
        }
        if parallel_min_segments < 2 || parallel_min_segments > max_segments {
            return Err(SegmentDirectoryError::InvalidPolicy(
                "parallel_min_segments",
            ));
        }
        Ok(Self {
            delta_max_rows,
            max_segments,
            compaction_fan_in,
            parallel_min_segments,
        })
    }

    /// Returns the maximum active-delta rows.
    #[must_use]
    pub const fn delta_max_rows(self) -> u64 {
        self.delta_max_rows
    }

    /// Returns the maximum number of segments searched by one query.
    #[must_use]
    pub const fn max_segments(self) -> usize {
        self.max_segments
    }

    /// Returns the maximum number of old segments read by one compaction.
    #[must_use]
    pub const fn compaction_fan_in(self) -> usize {
        self.compaction_fan_in
    }

    /// Returns the minimum fan-out at which parallel search may be admitted.
    #[must_use]
    pub const fn parallel_min_segments(self) -> usize {
        self.parallel_min_segments
    }
}

/// Immutable search snapshot protected by reader pins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectorySnapshot {
    generation: u64,
    segments: Vec<HnswSegment>,
    delta_rows: u64,
}

impl DirectorySnapshot {
    /// Returns the published directory generation.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns every segment that must be searched for this snapshot.
    #[must_use]
    pub fn segments(&self) -> &[HnswSegment] {
        &self.segments
    }

    /// Returns the exact active-delta rows that must also be scanned.
    #[must_use]
    pub const fn delta_rows(&self) -> u64 {
        self.delta_rows
    }
}

/// Bounded smallest-set compaction proposal; preparing it does not mutate the directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionPlan {
    source_generation: u64,
    segment_ids: Vec<SegmentId>,
    source_rows: u64,
    source_bytes: u64,
}

impl CompactionPlan {
    /// Returns the generation this plan is fenced against.
    #[must_use]
    pub const fn source_generation(&self) -> u64 {
        self.source_generation
    }

    /// Returns the bounded old segment set in deterministic order.
    #[must_use]
    pub fn segment_ids(&self) -> &[SegmentId] {
        &self.segment_ids
    }

    /// Returns the rows read by the compaction.
    #[must_use]
    pub const fn source_rows(&self) -> u64 {
        self.source_rows
    }

    /// Returns the bytes read by the compaction.
    #[must_use]
    pub const fn source_bytes(&self) -> u64 {
        self.source_bytes
    }
}

/// Atomic publication result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentPublication {
    /// Newly visible segment.
    pub published: HnswSegment,
    /// Superseded segments retained until their reader pins drain.
    pub retired: Vec<HnswSegment>,
}

/// Pure directory failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SegmentDirectoryError {
    /// Invalid hard-bound field.
    #[error("invalid segment policy: {0}")]
    InvalidPolicy(&'static str),
    /// Active delta would exceed its hard row cap.
    #[error("active HNSW delta is full")]
    DeltaFull,
    /// A full segment directory must compact before rotating another delta.
    #[error("segment compaction is required before rotation")]
    CompactionRequired,
    /// Rotation requires at least one active row and nonzero payload bytes.
    #[error("rotation has no materialized payload")]
    EmptyRotation,
    /// There are too few segments to compact.
    #[error("there are too few segments to compact")]
    NothingToCompact,
    /// The prepared compaction no longer matches the published directory.
    #[error("stale segment compaction plan")]
    StalePlan,
    /// A reader attempted to release a segment it did not pin.
    #[error("segment reader pin is absent")]
    MissingPin,
}

/// Published immutable segment directory plus one mutable bounded delta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentDirectory {
    policy: SegmentPolicy,
    generation: u64,
    next_segment_id: u64,
    segments: Vec<HnswSegment>,
    delta_rows: u64,
    pins: BTreeMap<SegmentId, u32>,
    retired: BTreeMap<SegmentId, HnswSegment>,
}

impl SegmentDirectory {
    /// Creates an empty directory.
    #[must_use]
    pub const fn new(policy: SegmentPolicy) -> Self {
        Self {
            policy,
            generation: 1,
            next_segment_id: 1,
            segments: Vec::new(),
            delta_rows: 0,
            pins: BTreeMap::new(),
            retired: BTreeMap::new(),
        }
    }

    /// Returns the current publication generation.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns all currently searchable segments.
    #[must_use]
    pub fn segments(&self) -> &[HnswSegment] {
        &self.segments
    }

    /// Returns active-delta rows.
    #[must_use]
    pub const fn delta_rows(&self) -> u64 {
        self.delta_rows
    }

    /// Returns deterministic foreground compaction debt.
    #[must_use]
    pub fn compaction_debt(&self) -> usize {
        if self.delta_rows == self.policy.delta_max_rows {
            self.segments
                .len()
                .saturating_add(1)
                .saturating_sub(self.policy.max_segments)
        } else {
            0
        }
    }

    /// Returns whether admitted parallel segment search is useful by count.
    #[must_use]
    pub fn parallel_search_candidate(&self) -> bool {
        self.segments.len() >= self.policy.parallel_min_segments
    }

    /// Appends logical rows to the active delta without exceeding its cap.
    ///
    /// # Errors
    ///
    /// Returns [`SegmentDirectoryError::DeltaFull`] without mutation when the
    /// addition would exceed the active-delta cap.
    pub fn append_delta_rows(&mut self, rows: u64) -> Result<(), SegmentDirectoryError> {
        let next = self
            .delta_rows
            .checked_add(rows)
            .ok_or(SegmentDirectoryError::DeltaFull)?;
        if next > self.policy.delta_max_rows {
            return Err(SegmentDirectoryError::DeltaFull);
        }
        self.delta_rows = next;
        Ok(())
    }

    /// Atomically rotates the active delta into one immutable segment.
    pub fn rotate_delta(
        &mut self,
        payload_bytes: u64,
    ) -> Result<HnswSegment, SegmentDirectoryError> {
        if self.delta_rows == 0 || payload_bytes == 0 {
            return Err(SegmentDirectoryError::EmptyRotation);
        }
        if self.segments.len() >= self.policy.max_segments {
            return Err(SegmentDirectoryError::CompactionRequired);
        }
        self.generation = self.generation.saturating_add(1);
        let segment = HnswSegment {
            id: SegmentId(self.next_segment_id),
            generation: self.generation,
            rows: self.delta_rows,
            bytes: payload_bytes,
        };
        self.next_segment_id = self.next_segment_id.saturating_add(1);
        self.segments.push(segment);
        self.delta_rows = 0;
        Ok(segment)
    }

    /// Pins and returns one exact old-valid search snapshot.
    pub fn pin_snapshot(&mut self) -> DirectorySnapshot {
        for segment in &self.segments {
            *self.pins.entry(segment.id).or_default() += 1;
        }
        DirectorySnapshot {
            generation: self.generation,
            segments: self.segments.clone(),
            delta_rows: self.delta_rows,
        }
    }

    /// Releases every segment pin held by a prior snapshot.
    pub fn unpin_snapshot(
        &mut self,
        snapshot: &DirectorySnapshot,
    ) -> Result<(), SegmentDirectoryError> {
        for segment in &snapshot.segments {
            let Some(count) = self.pins.get_mut(&segment.id) else {
                return Err(SegmentDirectoryError::MissingPin);
            };
            if *count == 1 {
                self.pins.remove(&segment.id);
            } else {
                *count -= 1;
            }
        }
        Ok(())
    }

    /// Prepares a deterministic bounded smallest-set compaction.
    pub fn prepare_compaction(&self) -> Result<CompactionPlan, SegmentDirectoryError> {
        if self.segments.len() < 2 {
            return Err(SegmentDirectoryError::NothingToCompact);
        }
        let mut selected = self.segments.clone();
        selected.sort_unstable_by_key(|segment| (segment.rows, segment.id));
        selected.truncate(self.policy.compaction_fan_in.min(selected.len()));
        selected.sort_unstable_by_key(|segment| segment.id);
        Ok(CompactionPlan {
            source_generation: self.generation,
            segment_ids: selected.iter().map(|segment| segment.id).collect(),
            source_rows: selected.iter().map(|segment| segment.rows).sum(),
            source_bytes: selected.iter().map(|segment| segment.bytes).sum(),
        })
    }

    /// Publishes a prepared compaction as one directory transition.
    pub fn publish_compaction(
        &mut self,
        plan: &CompactionPlan,
        output_rows: u64,
        output_bytes: u64,
    ) -> Result<SegmentPublication, SegmentDirectoryError> {
        if plan.source_generation != self.generation || output_bytes == 0 {
            return Err(SegmentDirectoryError::StalePlan);
        }
        let selected: BTreeSet<_> = plan.segment_ids.iter().copied().collect();
        if selected.len() != plan.segment_ids.len()
            || !selected
                .iter()
                .all(|id| self.segments.iter().any(|segment| segment.id == *id))
        {
            return Err(SegmentDirectoryError::StalePlan);
        }
        let mut retired = Vec::with_capacity(selected.len());
        self.segments.retain(|segment| {
            if selected.contains(&segment.id) {
                retired.push(*segment);
                false
            } else {
                true
            }
        });
        self.generation = self.generation.saturating_add(1);
        let published = HnswSegment {
            id: SegmentId(self.next_segment_id),
            generation: self.generation,
            rows: output_rows,
            bytes: output_bytes,
        };
        self.next_segment_id = self.next_segment_id.saturating_add(1);
        self.segments.push(published);
        self.segments.sort_unstable_by_key(|segment| segment.id);
        for segment in &retired {
            self.retired.insert(segment.id, *segment);
        }
        Ok(SegmentPublication { published, retired })
    }

    /// Returns retired segments whose reader pins have drained and removes
    /// them from the retirement inventory.
    pub fn reclaimable_segments(&mut self) -> Vec<HnswSegment> {
        let reclaimable: Vec<_> = self
            .retired
            .iter()
            .filter_map(|(id, segment)| (!self.pins.contains_key(id)).then_some((*id, *segment)))
            .collect();
        for (id, _) in &reclaimable {
            self.retired.remove(id);
        }
        reclaimable
            .into_iter()
            .map(|(_, segment)| segment)
            .collect()
    }
}
