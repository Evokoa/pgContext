// Thread-safe HNSW bulk-build support, included by `hnsw_hierarchy.rs`.

use std::sync::atomic::AtomicUsize;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// A poisoned lock means a worker thread panicked mid-wiring: the build's
/// state is unreliable, so every lock acquisition surfaces poisoning as
/// this typed error (aborting the whole build) instead of panicking again.
const POISONED_BUILD_LOCK: HnswError = HnswError::InvalidSnapshot {
    reason: "concurrent HNSW build lock poisoned by a panicked worker",
};

/// One node layer's mutable state: its neighbor list plus the backbone
/// pointers that protect the connectivity spine during pruning.
///
/// Keeping the backbone pointers in the same mutex as the neighbor list is
/// what makes per-node pruning sound: a prune always sees backbone pointers
/// consistent with the list it is about to rewrite.
#[derive(Debug)]
struct BuilderLayerState {
    neighbors: Vec<HnswNodeId>,
    backbone_previous: Option<HnswNodeId>,
    backbone_next: Option<HnswNodeId>,
}

/// Immutable-after-publication node storage for the concurrent builder.
///
/// `point_id` and `vector` never change once the slot is published, so
/// distance computations read them without any lock; only the per-layer
/// [`BuilderLayerState`] is mutable.
#[derive(Debug)]
struct BuilderSlot {
    point_id: HnswPointId,
    vector: DenseVector,
    layers: Vec<Mutex<BuilderLayerState>>,
}

/// Small shared state serialized by one mutex: id assignment, backbone
/// chain tails, entry point, and identity/dimension validation.
///
/// Everything here is a few pointer-sized writes — no distance math ever
/// runs while this lock is held, which is the difference from the earlier
/// whole-graph `RwLock` design that this replaces.
#[derive(Debug)]
struct BuilderRegistry {
    next_id: usize,
    layer_tails: [Option<HnswNodeId>; MAX_GRAPH_LAYERS],
    entry_point: Option<(HnswNodeId, HnswLevel)>,
    point_ids: BTreeSet<HnswPointId>,
    dimension: Option<usize>,
}

/// Outcome of one commit attempt against the registry.
enum CommitAttempt {
    /// The node was assigned an id, published, and chained.
    Committed {
        node_id: HnswNodeId,
        chain_previous: Vec<Option<HnswNodeId>>,
    },
    /// The plan assumed an empty graph but a concurrent insert committed
    /// first; the caller must re-plan against the now-nonempty graph.
    Replan(DenseVector),
}

/// Thread-safe HNSW graph builder for parallel bulk construction.
///
/// Uses per-node locking, not one whole-graph lock: each node's per-layer
/// neighbor list has its own mutex, vectors are immutable once published,
/// and a single small registry mutex serializes only id assignment, chain
/// linkage, and entry-point publication (pointer writes, no distance math).
/// Candidate search and reciprocal-neighbor pruning — the expensive parts
/// of HNSW insertion — therefore run concurrently across threads, holding
/// at most one node lock at a time (which also rules out lock-order
/// deadlocks by construction).
///
/// A graph built with multiple threads satisfies the same structural
/// invariants as a sequential build (reciprocity, bounded degree, layer
/// connectivity via the backbone spine, valid entry point) — re-validated
/// by [`Self::finish`] — but is **not** bit-identical to a sequential
/// build of the same rows: insertion interleaving affects exact neighbor
/// selection. With one effective worker the result is identical to
/// [`HnswGraph::insert`]'s.
pub struct ConcurrentHnswBuilder {
    metric: DistanceMetric,
    config: HnswConfig,
    level_seed: HnswLevelSeed,
    slots: Vec<OnceLock<BuilderSlot>>,
    registry: Mutex<BuilderRegistry>,
    level_counter: AtomicUsize,
}

impl ConcurrentHnswBuilder {
    /// Creates an empty concurrent builder for at most `capacity` nodes.
    ///
    /// The capacity is fixed up front (the bulk-build caller knows its row
    /// count) so node storage never reallocates while other threads hold
    /// references into it.
    #[must_use]
    pub fn new(metric: DistanceMetric, config: HnswConfig, capacity: usize) -> Self {
        let mut slots = Vec::with_capacity(capacity);
        slots.resize_with(capacity, OnceLock::new);
        Self {
            metric,
            config,
            level_seed: HnswLevelSeed::DEFAULT,
            slots,
            registry: Mutex::new(BuilderRegistry {
                next_id: 0,
                layer_tails: [None; MAX_GRAPH_LAYERS],
                entry_point: None,
                point_ids: BTreeSet::new(),
                dimension: None,
            }),
            level_counter: AtomicUsize::new(0),
        }
    }

    /// Inserts a vector. Safe to call from multiple threads concurrently.
    ///
    /// # Errors
    ///
    /// Returns the same duplicate-ID, dimension, metric, and bounded-work
    /// errors as [`HnswGraph::insert`], plus
    /// [`HnswError::InvalidParameter`] when the builder's fixed capacity is
    /// exceeded, plus [`HnswError::InvalidSnapshot`] when a lock was
    /// poisoned by a panicked worker thread (the build's state is
    /// unreliable, so poisoning surfaces as a typed error rather than a
    /// follow-on panic).
    pub fn insert(&self, point_id: HnswPointId, vector: DenseVector) -> Result<HnswNodeId> {
        ensure_hnsw_metric(self.metric)?;
        hnsw_distance(self.metric, &vector, &vector)?;
        {
            let mut registry = self.lock_registry()?;
            match registry.dimension {
                Some(dimension) if dimension != vector.dimension() => {
                    return Err(HnswError::DimensionMismatch {
                        left: dimension,
                        right: vector.dimension(),
                    });
                }
                Some(_) => {}
                None => registry.dimension = Some(vector.dimension()),
            }
            if !registry.point_ids.insert(point_id) {
                return Err(HnswError::DuplicatePointId { point_id });
            }
        }
        // The ordinal drives only the level distribution; node identity is
        // assigned at commit time so ids match backbone-chain order (the
        // property `HnswGraph::rebuild_hierarchy_backbone` assumes when a
        // snapshot of this graph is reloaded).
        let ordinal = self
            .level_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let level = deterministic_level(self.level_seed, HnswNodeId::new(ordinal), self.config.m());

        let mut work = HnswWork::default();
        let mut vector = vector;
        loop {
            let entry = self.lock_registry()?.entry_point;
            let planned = match entry {
                None => None,
                Some((entry_id, _)) => {
                    Some(self.plan_layers(&vector, level, entry_id, &mut work)?)
                }
            };
            match self.commit(point_id, vector, level, planned)? {
                CommitAttempt::Replan(returned) => {
                    vector = returned;
                    continue;
                }
                CommitAttempt::Committed {
                    node_id,
                    chain_previous,
                } => {
                    self.wire(node_id, level, &chain_previous, &mut work)?;
                    return Ok(node_id);
                }
            }
        }
    }

    /// Consumes the builder, re-validates every structural invariant, and
    /// returns the finished graph.
    ///
    /// # Errors
    ///
    /// Returns [`HnswError::InvalidSnapshot`] if the built graph violates a
    /// hierarchy invariant (the final safety net over concurrent wiring) or
    /// if a lock was poisoned by a panicked worker thread — a poisoned
    /// build's state is unreliable, so it surfaces as a typed error rather
    /// than a follow-on panic.
    pub fn finish(self) -> Result<HnswGraph> {
        let registry = self
            .registry
            .into_inner()
            .map_err(|_| POISONED_BUILD_LOCK)?;
        let count = registry.next_id;
        let mut nodes = Vec::with_capacity(count);
        for (index, slot) in self.slots.into_iter().take(count).enumerate() {
            let Some(slot) = slot.into_inner() else {
                return Err(HnswError::InvalidSnapshot {
                    reason: "concurrent build slot was never published",
                });
            };
            let layers = slot
                .layers
                .into_iter()
                .map(|layer| {
                    layer
                        .into_inner()
                        .map(|state| state.neighbors)
                        .map_err(|_| POISONED_BUILD_LOCK)
                })
                .collect::<Result<Vec<_>>>()?;
            nodes.push(HnswGraphNodeSnapshot {
                node_id: HnswNodeId::new(index),
                point_id: slot.point_id,
                vector: slot.vector,
                layers,
            });
        }
        Self::repair_reciprocity(&mut nodes);
        let snapshot = HnswGraphSnapshot {
            level_seed: self.level_seed,
            entry_point: registry.entry_point.map(|(node_id, _)| node_id),
            nodes,
        };
        HnswGraph::from_snapshot(self.metric, self.config, snapshot)
    }

    /// Drops any directed edge whose reverse edge is missing.
    ///
    /// [`Self::wire`] adds reciprocal edges from a `selected` list cloned
    /// under the node's lock, but the node's list can be re-pruned by a
    /// concurrent insert between that clone and the reciprocal add — the
    /// re-add then leaves `neighbor -> node` with no matching
    /// `node -> neighbor`. Rather than hold two node locks across the add
    /// (reintroducing lock-ordering risk), the asymmetry is repaired here,
    /// single-threaded, before validation. Backbone spine edges are pruned
    /// as protected on *both* sides, so they are always reciprocal by this
    /// point and layer connectivity through the spine is never affected;
    /// with one effective worker no asymmetry can occur and this is a
    /// no-op.
    fn repair_reciprocity(nodes: &mut [HnswGraphNodeSnapshot]) {
        let neighbor_sets: Vec<Vec<BTreeSet<HnswNodeId>>> = nodes
            .iter()
            .map(|node| {
                node.layers
                    .iter()
                    .map(|layer| layer.iter().copied().collect())
                    .collect()
            })
            .collect();
        for (index, node) in nodes.iter_mut().enumerate() {
            let this_id = HnswNodeId::new(index);
            for (layer_index, layer) in node.layers.iter_mut().enumerate() {
                layer.retain(|neighbor| {
                    neighbor_sets
                        .get(neighbor.get())
                        .and_then(|layers| layers.get(layer_index))
                        .is_some_and(|reverse| reverse.contains(&this_id))
                });
            }
        }
    }

    fn lock_registry(&self) -> Result<MutexGuard<'_, BuilderRegistry>> {
        self.registry.lock().map_err(|_| POISONED_BUILD_LOCK)
    }

    fn slot(&self, node_id: HnswNodeId) -> Result<&BuilderSlot> {
        self.slots
            .get(node_id.get())
            .and_then(OnceLock::get)
            .ok_or(HnswError::InvalidSnapshot {
                reason: "concurrent build reached an unpublished node",
            })
    }

    fn lock_layer(&self, node_id: HnswNodeId, layer: LayerIndex) -> Result<MutexGuard<'_, BuilderLayerState>> {
        let slot = self.slot(node_id)?;
        let state = slot.layers.get(layer.get()).ok_or(HnswError::InvalidSnapshot {
            reason: "concurrent build reached a layer the node does not own",
        })?;
        state.lock().map_err(|_| POISONED_BUILD_LOCK)
    }

    fn distance_to(&self, query: &DenseVector, node_id: HnswNodeId) -> Result<f32> {
        hnsw_distance(self.metric, query, &self.slot(node_id)?.vector)
    }

    /// Assigns the node id, publishes the slot (with the planned neighbor
    /// lists already in place, so a concurrent reciprocal edge added right
    /// after publication is never overwritten), links it into the backbone
    /// chain, and updates the entry point — all under one short registry
    /// lock so a published entry point or chain tail always refers to a
    /// readable slot. Only pointer writes and `Vec` moves happen here; no
    /// distance math ever runs under this lock.
    fn commit(
        &self,
        point_id: HnswPointId,
        vector: DenseVector,
        level: HnswLevel,
        planned: Option<Vec<Vec<HnswNodeId>>>,
    ) -> Result<CommitAttempt> {
        let mut registry = self.lock_registry()?;
        let Some(mut planned) = planned else {
            if registry.next_id > 0 {
                // Planned against an empty graph, but a concurrent insert
                // committed first — re-plan against the now-visible entry.
                return Ok(CommitAttempt::Replan(vector));
            }
            let id = registry.next_id;
            if id >= self.slots.len() {
                return Err(HnswError::InvalidParameter {
                    parameter: "concurrent build capacity",
                    value: id,
                });
            }
            registry.next_id += 1;
            let node_id = HnswNodeId::new(id);
            let layer_count = level.layer_count();
            let layers = (0..layer_count)
                .map(|_| {
                    Mutex::new(BuilderLayerState {
                        neighbors: Vec::new(),
                        backbone_previous: None,
                        backbone_next: None,
                    })
                })
                .collect();
            for layer_index in 0..layer_count {
                registry.layer_tails[layer_index] = Some(node_id);
            }
            if self
                .slots[id]
                .set(BuilderSlot {
                    point_id,
                    vector,
                    layers,
                })
                .is_err()
            {
                return Err(HnswError::InvalidSnapshot {
                    reason: "concurrent build published the same slot twice",
                });
            }
            registry.entry_point = Some((node_id, level));
            return Ok(CommitAttempt::Committed {
                node_id,
                chain_previous: vec![None; layer_count],
            });
        };
        let id = registry.next_id;
        if id >= self.slots.len() {
            return Err(HnswError::InvalidParameter {
                parameter: "concurrent build capacity",
                value: id,
            });
        }
        registry.next_id += 1;
        let node_id = HnswNodeId::new(id);
        let layer_count = level.layer_count();
        planned.resize(layer_count, Vec::new());
        let mut chain_previous = Vec::with_capacity(layer_count);
        let mut layers = Vec::with_capacity(layer_count);
        for (layer_index, planned_neighbors) in planned.into_iter().enumerate() {
            let previous = registry.layer_tails[layer_index];
            chain_previous.push(previous);
            layers.push(Mutex::new(BuilderLayerState {
                neighbors: planned_neighbors,
                backbone_previous: previous,
                backbone_next: None,
            }));
            registry.layer_tails[layer_index] = Some(node_id);
        }
        if self.slots[id]
            .set(BuilderSlot {
                point_id,
                vector,
                layers,
            })
            .is_err()
        {
            return Err(HnswError::InvalidSnapshot {
                reason: "concurrent build published the same slot twice",
            });
        }
        let replaces_entry = match registry.entry_point {
            None => true,
            Some((_, current_level)) => level > current_level,
        };
        if replaces_entry {
            registry.entry_point = Some((node_id, level));
        }
        Ok(CommitAttempt::Committed {
            node_id,
            chain_previous,
        })
    }

    /// Wires the committed node into the graph: backbone pointers first
    /// (so predecessors protect the spine edges about to be created), then
    /// the planned neighbor edges with reciprocal pruning — the same
    /// operation order as the sequential `commit_insertion`, but holding
    /// only one node lock at a time.
    fn wire(
        &self,
        node_id: HnswNodeId,
        level: HnswLevel,
        chain_previous: &[Option<HnswNodeId>],
        work: &mut HnswWork,
    ) -> Result<()> {
        for (layer_index, previous) in chain_previous.iter().enumerate() {
            let layer = LayerIndex::new(layer_index);
            if let Some(previous) = *previous {
                self.lock_layer(previous, layer)?.backbone_next = Some(node_id);
            }
        }
        for layer_index in 0..level.layer_count() {
            let layer = LayerIndex::new(layer_index);
            if let Some(previous) = chain_previous.get(layer_index).copied().flatten() {
                let mut state = self.lock_layer(node_id, layer)?;
                if !state.neighbors.contains(&previous) {
                    state.neighbors.push(previous);
                }
            }
            let dropped = self.prune(node_id, layer, work)?;
            self.remove_back_edges(node_id, layer, &dropped)?;
            let selected = self.lock_layer(node_id, layer)?.neighbors.clone();
            for neighbor in selected {
                {
                    let mut state = self.lock_layer(neighbor, layer)?;
                    if !state.neighbors.contains(&node_id) {
                        state.neighbors.push(node_id);
                    }
                }
                let dropped = self.prune(neighbor, layer, work)?;
                self.remove_back_edges(neighbor, layer, &dropped)?;
            }
        }
        Ok(())
    }

    /// Rewrites one node layer down to its connection budget, protecting
    /// its backbone spine edges, and returns the dropped neighbor ids so
    /// the caller can remove the reciprocal back-edges (one lock at a
    /// time, after this node's lock is released).
    fn prune(
        &self,
        node_id: HnswNodeId,
        layer: LayerIndex,
        work: &mut HnswWork,
    ) -> Result<Vec<HnswNodeId>> {
        let max_connections = self.config.max_connections(layer);
        let query = {
            let state = self.lock_layer(node_id, layer)?;
            if state.neighbors.len() <= max_connections {
                return Ok(Vec::new());
            }
            drop(state);
            self.slot(node_id)?.vector.clone()
        };
        let mut state = self.lock_layer(node_id, layer)?;
        if state.neighbors.len() <= max_connections {
            return Ok(Vec::new());
        }
        let protected = [state.backbone_previous, state.backbone_next]
            .into_iter()
            .flatten()
            .collect::<BTreeSet<_>>();
        let mut candidates = Vec::with_capacity(state.neighbors.len());
        for neighbor_id in &state.neighbors {
            work.record_distance()?;
            candidates.push(Candidate {
                node_id: *neighbor_id,
                score: self.distance_to(&query, *neighbor_id)?,
            });
        }
        sort_candidates(&mut candidates);
        let mut kept = self.select_neighbors(&query, &candidates, max_connections, work)?;
        for protected_id in protected.iter().copied() {
            if kept.contains(&protected_id)
                || !candidates
                    .iter()
                    .any(|candidate| candidate.node_id == protected_id)
            {
                continue;
            }
            if kept.len() == max_connections
                && let Some(position) = kept
                    .iter()
                    .rposition(|candidate| !protected.contains(candidate))
            {
                kept.remove(position);
            }
            if kept.len() < max_connections {
                kept.push(protected_id);
            }
        }
        let removed = candidates
            .iter()
            .map(|candidate| candidate.node_id)
            .filter(|candidate| !kept.contains(candidate))
            .collect::<Vec<_>>();
        state.neighbors = kept;
        Ok(removed)
    }

    fn remove_back_edges(
        &self,
        node_id: HnswNodeId,
        layer: LayerIndex,
        dropped: &[HnswNodeId],
    ) -> Result<()> {
        for dropped_id in dropped {
            self.lock_layer(*dropped_id, layer)?
                .neighbors
                .retain(|neighbor| *neighbor != node_id);
        }
        Ok(())
    }

    /// Read-only insertion planning: descends from the entry point and
    /// selects per-layer neighbors, mirroring the sequential
    /// `plan_insertion` but over the concurrent storage. Runs with no lock
    /// held except brief per-node neighbor-list reads.
    fn plan_layers(
        &self,
        query: &DenseVector,
        level: HnswLevel,
        entry_id: HnswNodeId,
        work: &mut HnswWork,
    ) -> Result<Vec<Vec<HnswNodeId>>> {
        let entry_level = HnswLevel::new(
            self.slot(entry_id)?.layers.len().saturating_sub(1),
        )?;
        let mut current = entry_id;
        if entry_level > level {
            for layer_index in ((level.get() + 1)..=entry_level.get()).rev() {
                let candidates =
                    self.search_layer(query, current, 1, LayerIndex::new(layer_index), work)?;
                if let Some(best) = candidates.first() {
                    current = best.node_id;
                }
            }
        }
        let shared_max = level.get().min(entry_level.get());
        let mut layers = vec![Vec::new(); level.layer_count()];
        for layer_index in (0..=shared_max).rev() {
            let layer = LayerIndex::new(layer_index);
            let candidates =
                self.search_layer(query, current, self.config.ef_construction(), layer, work)?;
            let max_connections = self.config.max_connections(layer);
            layers[layer_index] =
                self.select_neighbors(query, &candidates, max_connections, work)?;
            if let Some(best) = candidates.first() {
                current = best.node_id;
            }
        }
        Ok(layers)
    }

    /// Bounded best-first search of one layer, mirroring the sequential
    /// `search_layer_candidates` over concurrent storage: neighbor lists
    /// are cloned under their per-node lock, distances read immutable
    /// published vectors with no lock held.
    fn search_layer(
        &self,
        query: &DenseVector,
        entry: HnswNodeId,
        ef: usize,
        layer: LayerIndex,
        work: &mut HnswWork,
    ) -> Result<Vec<Candidate>> {
        work.record_distance()?;
        let entry_candidate = Candidate {
            node_id: entry,
            score: self.distance_to(query, entry)?,
        };
        let mut pending = BinaryHeap::from([Reverse(entry_candidate)]);
        let mut nearest = BinaryHeap::from([entry_candidate]);
        let mut visited = vec![false; self.slots.len()];
        let Some(entry_visited) = visited.get_mut(entry.get()) else {
            return Err(HnswError::InvalidSnapshot {
                reason: "entry point exceeds graph node count",
            });
        };
        *entry_visited = true;

        while let Some(Reverse(candidate)) = pending.pop() {
            work.record_expansion()?;
            let worst = nearest.peek().map_or(f32::INFINITY, |item| item.score);
            if nearest.len() >= ef && candidate.score > worst {
                break;
            }
            let neighbors = match self.slot(candidate.node_id)?.layers.get(layer.get()) {
                Some(state) => {
                    state
                        .lock()
                        .map_err(|_| POISONED_BUILD_LOCK)?
                        .neighbors
                        .clone()
                }
                None => Vec::new(),
            };
            for neighbor in neighbors {
                work.record_edge()?;
                let Some(neighbor_visited) = visited.get_mut(neighbor.get()) else {
                    return Err(HnswError::InvalidSnapshot {
                        reason: "neighbor exceeds graph node count",
                    });
                };
                if *neighbor_visited {
                    continue;
                }
                *neighbor_visited = true;
                work.record_distance()?;
                let scored = Candidate {
                    node_id: neighbor,
                    score: self.distance_to(query, neighbor)?,
                };
                let should_add = nearest.len() < ef
                    || nearest
                        .peek()
                        .is_some_and(|current_worst| scored < *current_worst);
                if should_add {
                    pending.push(Reverse(scored));
                    nearest.push(scored);
                    if nearest.len() > ef {
                        nearest.pop();
                    }
                }
            }
        }
        Ok(nearest.into_sorted_vec())
    }

    /// Diversity-preferring neighbor selection, mirroring the sequential
    /// `select_neighbors_heuristic` over concurrent storage.
    fn select_neighbors(
        &self,
        query: &DenseVector,
        candidates: &[Candidate],
        maximum: usize,
        work: &mut HnswWork,
    ) -> Result<Vec<HnswNodeId>> {
        let mut selected: Vec<HnswNodeId> = Vec::with_capacity(maximum.min(candidates.len()));
        let mut rejected = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let candidate_vector = &self.slot(candidate.node_id)?.vector;
            let mut diverse = true;
            for selected_id in &selected {
                work.record_distance()?;
                let separation =
                    hnsw_distance(self.metric, candidate_vector, &self.slot(*selected_id)?.vector)?;
                if separation < candidate.score {
                    diverse = false;
                    break;
                }
            }
            if diverse {
                selected.push(candidate.node_id);
                if selected.len() == maximum {
                    break;
                }
            } else {
                rejected.push(candidate.node_id);
            }
        }
        if selected.len() < maximum {
            selected.extend(rejected.into_iter().take(maximum - selected.len()));
        }
        if let Some(first) = selected.first() {
            let dimension = self.slot(*first)?.vector.dimension();
            if dimension != query.dimension() {
                return Err(HnswError::DimensionMismatch {
                    left: dimension,
                    right: query.dimension(),
                });
            }
        }
        Ok(selected)
    }
}
