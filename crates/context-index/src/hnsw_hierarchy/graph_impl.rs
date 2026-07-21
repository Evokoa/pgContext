impl HnswGraph {
    /// Creates an empty graph with explicit deterministic level seed.
    #[must_use]
    pub const fn with_level_seed(
        metric: DistanceMetric,
        config: HnswConfig,
        level_seed: HnswLevelSeed,
    ) -> Self {
        Self {
            metric,
            config,
            dimension: None,
            entry_point: None,
            nodes: Vec::new(),
            point_ids: BTreeSet::new(),
            level_seed,
            layer_tails: [None; MAX_GRAPH_LAYERS],
        }
    }

    /// Returns a node's maximum participating level.
    #[must_use]
    pub fn node_level(&self, node_id: HnswNodeId) -> Option<HnswLevel> {
        self.nodes
            .get(node_id.get())
            .and_then(|node| HnswLevel::new(node.layers.len().saturating_sub(1)).ok())
    }

    /// Returns the graph's current maximum level.
    #[must_use]
    pub fn max_level(&self) -> Option<HnswLevel> {
        self.nodes
            .iter()
            .filter_map(|node| HnswLevel::new(node.layers.len().saturating_sub(1)).ok())
            .max()
    }

    pub(crate) fn assigned_level(&self, node_id: HnswNodeId) -> HnswLevel {
        deterministic_level(self.level_seed, node_id, self.config.m())
    }

    /// Inserts a node at an explicit level for deterministic restore/testing.
    ///
    /// # Errors
    ///
    /// Returns a duplicate-ID, dimension, metric, cancellation, or bounded-work
    /// error without publishing a partial node.
    pub fn insert_at_level(
        &mut self,
        point_id: HnswPointId,
        vector: DenseVector,
        level: HnswLevel,
    ) -> Result<HnswInsertOutcome> {
        self.insert_at_level_with_control(point_id, vector, level, &mut NeverCancel)
    }

    /// Inserts at an explicit level with deterministic cancellation checkpoints.
    ///
    /// # Errors
    ///
    /// Returns a duplicate-ID, dimension, metric, cancellation, or bounded-work
    /// error without publishing a partial node.
    pub fn insert_at_level_with_control(
        &mut self,
        point_id: HnswPointId,
        vector: DenseVector,
        level: HnswLevel,
        cancellation: &mut impl HnswCancellation,
    ) -> Result<HnswInsertOutcome> {
        self.validate_insertion(point_id, &vector)?;
        let mut work = HnswWork::default();
        work.check_cancellation(cancellation)?;
        let plan = self.plan_insertion(&vector, level, &mut work, cancellation)?;
        self.commit_insertion(point_id, vector, level, plan, &mut work)
    }

    /// Validates a candidate insertion against the graph's current identity
    /// and dimension without mutating anything.
    ///
    /// Shared by the sequential [`Self::insert_at_level_with_control`] and
    /// the concurrent build path, which validates once per call but plans
    /// and commits as two separate, differently-locked steps.
    ///
    /// # Errors
    ///
    /// Returns [`HnswError::DuplicatePointId`] when `point_id` already
    /// exists, [`HnswError::DimensionMismatch`] when `vector`'s dimension
    /// differs from the graph's established dimension, or
    /// [`HnswError::Core`] when the metric rejects the vector.
    pub(crate) fn validate_insertion(
        &self,
        point_id: HnswPointId,
        vector: &DenseVector,
    ) -> Result<()> {
        ensure_hnsw_metric(self.metric)?;
        if self.point_ids.contains(&point_id) {
            return Err(HnswError::DuplicatePointId { point_id });
        }
        if let Some(dimension) = self.dimension
            && dimension != vector.dimension()
        {
            return Err(HnswError::DimensionMismatch {
                left: dimension,
                right: vector.dimension(),
            });
        }
        hnsw_distance(self.metric, vector, vector)?;
        Ok(())
    }

    /// Computes the read-only portion of an insertion: the descent path and
    /// per-layer neighbor selection for `vector` at `level` against the
    /// graph's current state.
    ///
    /// Read-only (`&self`) so multiple threads may plan concurrently (for
    /// example under a shared `RwLock` read guard) while another thread
    /// commits a different insertion. The returned plan may be slightly
    /// stale by the time it is committed if the graph changed in between —
    /// safe (every referenced node ID remains valid, since build-time
    /// graphs are append-only) but not necessarily optimal, matching how
    /// other concurrent HNSW implementations trade a little candidate
    /// freshness for parallelism.
    ///
    /// # Errors
    ///
    /// Returns [`HnswError::Core`] on distance computation failure, or a
    /// cancellation/bounded-work error from `cancellation`.
    pub(crate) fn plan_insertion(
        &self,
        vector: &DenseVector,
        level: HnswLevel,
        work: &mut HnswWork,
        cancellation: &mut impl HnswCancellation,
    ) -> Result<InsertionPlan> {
        let Some(mut current) = self.entry_point else {
            return Ok(InsertionPlan::FirstNode);
        };
        let entry_level = self.node_level(current).unwrap_or_else(HnswLevel::base);

        if entry_level > level {
            for layer_index in ((level.get() + 1)..=entry_level.get()).rev() {
                let candidates = self.search_layer_candidates(
                    vector,
                    current,
                    1,
                    LayerIndex::new(layer_index),
                    work,
                    cancellation,
                )?;
                if let Some(best) = candidates.first() {
                    current = best.node_id;
                }
            }
        }

        let shared_max = level.get().min(entry_level.get());
        let mut layers = vec![Vec::new(); level.layer_count()];
        for layer_index in (0..=shared_max).rev() {
            let layer = LayerIndex::new(layer_index);
            let candidates = self.search_layer_candidates(
                vector,
                current,
                self.config.ef_construction(),
                layer,
                work,
                cancellation,
            )?;
            let max_connections = self.config.max_connections(layer);
            layers[layer_index] =
                self.select_neighbors_heuristic(vector, &candidates, max_connections, work)?;
            if let Some(best) = candidates.first() {
                current = best.node_id;
            }
        }
        Ok(InsertionPlan::Candidates { layers })
    }

    /// Commits an insertion planned by [`Self::plan_insertion`], mutating
    /// the graph.
    ///
    /// Requires exclusive access (`&mut self`) — under a shared `RwLock`
    /// this is the write-locked half of a concurrent insert. Re-plans
    /// in-place if `plan` was computed while the graph was empty but is no
    /// longer empty by commit time (the only case a stale plan is unsafe to
    /// commit directly, since a first-node plan carries no candidates).
    ///
    /// # Errors
    ///
    /// Returns [`HnswError::Core`] on distance computation failure, or a
    /// cancellation/bounded-work error from `cancellation` when this method
    /// has to re-plan.
    pub(crate) fn commit_insertion(
        &mut self,
        point_id: HnswPointId,
        vector: DenseVector,
        level: HnswLevel,
        plan: InsertionPlan,
        work: &mut HnswWork,
    ) -> Result<HnswInsertOutcome> {
        let node_id = HnswNodeId::new(self.nodes.len());

        let Some(_current) = self.entry_point else {
            self.dimension = Some(vector.dimension());
            self.nodes.push(HnswNode {
                point_id,
                vector,
                layers: vec![Vec::new(); level.layer_count()],
                backbone_previous: vec![None; level.layer_count()],
                backbone_next: vec![None; level.layer_count()],
            });
            self.point_ids.insert(point_id);
            for layer_index in 0..level.layer_count() {
                self.layer_tails[layer_index] = Some(node_id);
            }
            self.entry_point = Some(node_id);
            return Ok(HnswInsertOutcome { node_id, work: *work });
        };

        let layers = match plan {
            InsertionPlan::Candidates { layers, .. } => layers,
            InsertionPlan::FirstNode => {
                // The graph was empty when this was planned but has since
                // gained an entry point from a concurrent commit; the plan
                // carries no candidates to reuse, so recompute against the
                // current state under this exclusive lock.
                match self.plan_insertion(&vector, level, work, &mut NeverCancel)? {
                    InsertionPlan::Candidates { layers, .. } => layers,
                    InsertionPlan::FirstNode => vec![Vec::new(); level.layer_count()],
                }
            }
        };

        let backbone_previous = (0..level.layer_count())
            .map(|layer_index| self.layer_tails[layer_index])
            .collect::<Vec<_>>();
        self.nodes.push(HnswNode {
            point_id,
            vector,
            layers,
            backbone_previous,
            backbone_next: vec![None; level.layer_count()],
        });
        self.point_ids.insert(point_id);
        for layer_index in 0..level.layer_count() {
            if let Some(previous) = self.layer_tails[layer_index]
                && let Some(next) = self
                    .nodes
                    .get_mut(previous.get())
                    .and_then(|node| node.backbone_next.get_mut(layer_index))
            {
                *next = Some(node_id);
            }
            self.layer_tails[layer_index] = Some(node_id);
        }
        // Bounded by the node's own layer count, not `shared_max`: the
        // backbone-chain predecessor recorded just above is authoritative
        // for whether a layer has a real prior participant to link against,
        // independent of how many layers `plan_insertion` could search at
        // plan time. Under concurrent commits, another thread may have
        // advanced `layer_tails` for a layer past `shared_max` between this
        // insertion's plan and commit; skipping the actual edge add here
        // (while still recording `backbone_previous`/`backbone_next` above)
        // would leave that layer's backbone metadata pointing at a real
        // predecessor with no corresponding graph edge, disconnecting the
        // induced layer. Layers with no real predecessor (`backbone_previous`
        // is `None`) fall through every `if let Some` below as a no-op, so
        // this is a pure widening: safe and behavior-preserving for the
        // sequential path, where `shared_max` already covered every layer
        // with a real predecessor.
        for layer_index in 0..level.layer_count() {
            let layer = LayerIndex::new(layer_index);
            if let Some(previous) = self.nodes[node_id.get()].backbone_previous[layer_index] {
                self.add_neighbor_on_layer(node_id, previous, layer);
            }
            self.prune_neighbors_on_layer(node_id, layer, work)?;
            let selected = self
                .neighbors(node_id, layer)
                .map(<[HnswNodeId]>::to_vec)
                .unwrap_or_default();
            for neighbor in selected {
                self.add_neighbor_on_layer(neighbor, node_id, layer);
                self.prune_neighbors_on_layer(neighbor, layer, work)?;
            }
        }
        if self
            .entry_point
            .and_then(|entry| self.node_level(entry))
            .is_none_or(|entry_level| level > entry_level)
        {
            self.entry_point = Some(node_id);
        }
        Ok(HnswInsertOutcome { node_id, work: *work })
    }

    /// Returns full typed hierarchy state for pure roundtrip/property use.
    #[must_use]
    pub fn snapshot(&self) -> HnswGraphSnapshot {
        HnswGraphSnapshot {
            level_seed: self.level_seed,
            entry_point: self.entry_point,
            nodes: self
                .nodes
                .iter()
                .enumerate()
                .map(|(node_id, node)| HnswGraphNodeSnapshot {
                    node_id: HnswNodeId::new(node_id),
                    point_id: node.point_id,
                    vector: node.vector.clone(),
                    layers: node.layers.clone(),
                })
                .collect(),
        }
    }

    /// Restores and validates full typed hierarchy state.
    ///
    /// # Errors
    ///
    /// Returns [`HnswError::InvalidSnapshot`] unless IDs, dimensions, levels,
    /// links, entry point, reciprocity, and induced-layer connectivity hold.
    pub fn from_snapshot(
        metric: DistanceMetric,
        config: HnswConfig,
        snapshot: HnswGraphSnapshot,
    ) -> Result<Self> {
        let mut point_ids = BTreeSet::new();
        for (expected_id, node) in snapshot.nodes.iter().enumerate() {
            if node.node_id != HnswNodeId::new(expected_id) {
                return Err(snapshot_error("node ids are not contiguous"));
            }
            if !point_ids.insert(node.point_id) {
                return Err(snapshot_error("point ids are not unique"));
            }
        }
        let mut graph = Self::with_level_seed(metric, config, snapshot.level_seed);
        graph.entry_point = snapshot.entry_point;
        graph.nodes = snapshot
            .nodes
            .into_iter()
            .map(|snapshot| HnswNode {
                point_id: snapshot.point_id,
                vector: snapshot.vector,
                layers: snapshot.layers,
                backbone_previous: Vec::new(),
                backbone_next: Vec::new(),
            })
            .collect();
        graph.point_ids = point_ids;
        if let Some(first) = graph.nodes.first() {
            graph.dimension = Some(first.vector.dimension());
        }
        graph.rebuild_hierarchy_backbone();
        graph.validate_snapshot()?;
        Ok(graph)
    }

    /// Searches with reusable mask, work evidence, and cancellation.
    ///
    /// # Errors
    ///
    /// Returns a dimension, mask-budget, metric, or cancellation error.
    pub fn search_with_control(
        &self,
        query: &DenseVector,
        limit: SearchLimit,
        mask: &CandidateMask,
        cancellation: &mut impl HnswCancellation,
    ) -> Result<HnswSearchOutcome> {
        mask.validate_budget()?;
        let (results, work) =
            self.search_hierarchical(query, limit, |point_id| mask.allows(point_id), cancellation)?;
        Ok(HnswSearchOutcome { results, work })
    }

    pub(crate) fn search_hierarchical(
        &self,
        query: &DenseVector,
        limit: SearchLimit,
        allows: impl Fn(HnswPointId) -> bool,
        cancellation: &mut impl HnswCancellation,
    ) -> Result<(Vec<HnswSearchResult>, HnswWork)> {
        ensure_hnsw_metric(self.metric)?;
        if self.nodes.is_empty() {
            return Ok((Vec::new(), HnswWork::default()));
        }
        self.ensure_query_dimension(query)?;
        let mut work = HnswWork::default();
        work.check_cancellation(cancellation)?;
        let mut current = self.entry_point.ok_or(HnswError::InvalidSnapshot {
            reason: "nonempty graph has no entry point",
        })?;
        let entry_level = self.node_level(current).ok_or(HnswError::InvalidSnapshot {
            reason: "entry point is missing a level",
        })?;
        for layer_index in (1..=entry_level.get()).rev() {
            let candidates = self.search_layer_candidates(
                query,
                current,
                1,
                LayerIndex::new(layer_index),
                &mut work,
                cancellation,
            )?;
            if let Some(best) = candidates.first() {
                current = best.node_id;
            }
        }
        let candidates = self.search_layer_candidates_filtered(
            query,
            current,
            self.config.ef_search().max(limit.get()),
            LayerIndex::base(),
            &allows,
            &mut work,
            cancellation,
        )?;
        let mut results = candidates
            .into_iter()
            .filter_map(|candidate| {
                let node = self.nodes.get(candidate.node_id.get())?;
                Some(HnswSearchResult {
                    point_id: node.point_id,
                    score: candidate.score,
                })
            })
            .collect::<Vec<_>>();
        sort_search_results(&mut results);
        results.truncate(limit.get());
        Ok((results, work))
    }

    fn search_layer_candidates(
        &self,
        query: &DenseVector,
        entry: HnswNodeId,
        ef: usize,
        layer: LayerIndex,
        work: &mut HnswWork,
        cancellation: &mut impl HnswCancellation,
    ) -> Result<Vec<Candidate>> {
        work.record_distance()?;
        let entry_candidate = Candidate {
            node_id: entry,
            score: self.distance_to_node(query, entry)?,
        };
        let mut pending = BinaryHeap::from([Reverse(entry_candidate)]);
        let mut nearest = BinaryHeap::from([entry_candidate]);
        let mut visited = vec![false; self.nodes.len()];
        let Some(entry_visited) = visited.get_mut(entry.get()) else {
            return Err(HnswError::InvalidSnapshot {
                reason: "entry point exceeds graph node count",
            });
        };
        *entry_visited = true;

        while let Some(Reverse(candidate)) = pending.pop() {
            work.check_cancellation(cancellation)?;
            work.record_expansion()?;
            let worst = nearest.peek().map_or(f32::INFINITY, |item| item.score);
            if nearest.len() >= ef && candidate.score > worst {
                break;
            }
            for neighbor in self.neighbors(candidate.node_id, layer).unwrap_or_default() {
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
                    node_id: *neighbor,
                    score: self.distance_to_node(query, *neighbor)?,
                };
                let should_add = nearest.len() < ef
                    || nearest.peek().is_some_and(|current_worst| scored < *current_worst);
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

    #[allow(clippy::too_many_arguments)]
    fn search_layer_candidates_filtered(
        &self,
        query: &DenseVector,
        entry: HnswNodeId,
        ef: usize,
        layer: LayerIndex,
        allows: &impl Fn(HnswPointId) -> bool,
        work: &mut HnswWork,
        cancellation: &mut impl HnswCancellation,
    ) -> Result<Vec<Candidate>> {
        work.record_distance()?;
        let entry_candidate = Candidate {
            node_id: entry,
            score: self.distance_to_node(query, entry)?,
        };
        let mut pending = BinaryHeap::from([Reverse(entry_candidate)]);
        let mut nearest = BinaryHeap::new();
        if self
            .nodes
            .get(entry.get())
            .is_some_and(|node| allows(node.point_id))
        {
            nearest.push(entry_candidate);
        }
        let mut visited = vec![false; self.nodes.len()];
        let Some(entry_visited) = visited.get_mut(entry.get()) else {
            return Err(HnswError::InvalidSnapshot {
                reason: "entry point exceeds graph node count",
            });
        };
        *entry_visited = true;

        while let Some(Reverse(candidate)) = pending.pop() {
            work.check_cancellation(cancellation)?;
            work.record_expansion()?;
            let worst = nearest.peek().map_or(f32::INFINITY, |item| item.score);
            if nearest.len() >= ef && candidate.score > worst {
                break;
            }
            for neighbor in self.neighbors(candidate.node_id, layer).unwrap_or_default() {
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
                    node_id: *neighbor,
                    score: self.distance_to_node(query, *neighbor)?,
                };
                if self
                    .nodes
                    .get(neighbor.get())
                    .is_some_and(|node| allows(node.point_id))
                {
                    nearest.push(scored);
                    if nearest.len() > ef {
                        nearest.pop();
                    }
                }
                let current_worst = nearest.peek().map_or(f32::INFINITY, |item| item.score);
                if nearest.len() < ef || scored.score <= current_worst {
                    pending.push(Reverse(scored));
                }
            }
        }
        Ok(nearest.into_sorted_vec())
    }

    fn add_neighbor_on_layer(
        &mut self,
        node_id: HnswNodeId,
        neighbor: HnswNodeId,
        layer: LayerIndex,
    ) {
        let Some(neighbors) = self
            .nodes
            .get_mut(node_id.get())
            .and_then(|node| node.layers.get_mut(layer.get()))
        else {
            return;
        };
        if !neighbors.contains(&neighbor) {
            neighbors.push(neighbor);
        }
    }

    fn select_neighbors_heuristic(
        &self,
        query: &DenseVector,
        candidates: &[Candidate],
        maximum: usize,
        work: &mut HnswWork,
    ) -> Result<Vec<HnswNodeId>> {
        let mut selected: Vec<HnswNodeId> =
            Vec::with_capacity(maximum.min(candidates.len()));
        let mut rejected = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let Some(candidate_node) = self.nodes.get(candidate.node_id.get()) else {
                return Err(HnswError::InvalidSnapshot {
                    reason: "neighbor candidate node is missing",
                });
            };
            let mut diverse = true;
            for selected_id in &selected {
                let Some(selected_node) = self.nodes.get(selected_id.get()) else {
                    return Err(HnswError::InvalidSnapshot {
                        reason: "selected neighbor node is missing",
                    });
                };
                work.record_distance()?;
                let separation =
                    hnsw_distance(self.metric, &candidate_node.vector, &selected_node.vector)?;
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
        // The query is consumed only as the origin represented by candidate
        // scores. Keep the typed argument to make that contract explicit and
        // verify its dimensions remain compatible with selected nodes.
        if let Some(first) = selected.first() {
            let Some(node) = self.nodes.get(first.get()) else {
                return Err(HnswError::InvalidSnapshot {
                    reason: "selected neighbor node is missing",
                });
            };
            if node.vector.dimension() != query.dimension() {
                return Err(HnswError::DimensionMismatch {
                    left: node.vector.dimension(),
                    right: query.dimension(),
                });
            }
        }
        Ok(selected)
    }

    fn prune_neighbors_on_layer(
        &mut self,
        node_id: HnswNodeId,
        layer: LayerIndex,
        work: &mut HnswWork,
    ) -> Result<()> {
        let neighbor_ids = self
            .neighbors(node_id, layer)
            .map(<[HnswNodeId]>::to_vec)
            .unwrap_or_default();
        let max_connections = self.config.max_connections(layer);
        if neighbor_ids.len() <= max_connections {
            return Ok(());
        }
        let Some(node) = self.nodes.get(node_id.get()) else {
            return Ok(());
        };
        let query = node.vector.clone();
        let protected = [
            node.backbone_previous.get(layer.get()).copied().flatten(),
            node.backbone_next.get(layer.get()).copied().flatten(),
        ]
        .into_iter()
        .flatten()
        .collect::<BTreeSet<_>>();
        let mut candidates = Vec::with_capacity(neighbor_ids.len());
        for neighbor_id in neighbor_ids {
            let Some(neighbor) = self.nodes.get(neighbor_id.get()) else {
                continue;
            };
            work.record_distance()?;
            candidates.push(Candidate {
                node_id: neighbor_id,
                score: hnsw_distance(self.metric, &node.vector, &neighbor.vector)?,
            });
        }
        sort_candidates(&mut candidates);
        let mut kept =
            self.select_neighbors_heuristic(&query, &candidates, max_connections, work)?;
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
        if let Some(neighbors) = self
            .nodes
            .get_mut(node_id.get())
            .and_then(|node| node.layers.get_mut(layer.get()))
        {
            *neighbors = kept;
        }
        for removed_id in removed {
            if let Some(reverse) = self
                .nodes
                .get_mut(removed_id.get())
                .and_then(|node| node.layers.get_mut(layer.get()))
            {
                reverse.retain(|neighbor| *neighbor != node_id);
            }
        }
        Ok(())
    }

    pub(crate) fn rebuild_hierarchy_backbone(&mut self) {
        self.layer_tails = [None; MAX_GRAPH_LAYERS];
        for node_index in 0..self.nodes.len() {
            let node_id = HnswNodeId::new(node_index);
            let layer_count = self.nodes[node_index].layers.len();
            self.nodes[node_index].backbone_previous = vec![None; layer_count];
            self.nodes[node_index].backbone_next = vec![None; layer_count];
            for layer_index in 0..layer_count {
                let previous = self.layer_tails[layer_index];
                self.nodes[node_index].backbone_previous[layer_index] = previous;
                if let Some(previous) = previous {
                    self.nodes[previous.get()].backbone_next[layer_index] = Some(node_id);
                }
                self.layer_tails[layer_index] = Some(node_id);
            }
        }
    }

    fn validate_snapshot(&mut self) -> Result<()> {
        let node_count = self.nodes.len();
        let dimension = self.dimension;
        for (node_id, node) in self.nodes.iter().enumerate() {
            if dimension != Some(node.vector.dimension()) {
                return Err(snapshot_error("vector dimensions differ"));
            }
            if node.layers.is_empty() || node.layers.len() > MAX_GRAPH_LAYERS {
                return Err(snapshot_error("layer count is outside bounds"));
            }
            for (layer_index, neighbors) in node.layers.iter().enumerate() {
                if neighbors.len()
                    > self.config.max_connections(LayerIndex::new(layer_index))
                {
                    return Err(snapshot_error("neighbor count exceeds graph policy"));
                }
                let mut unique = BTreeSet::new();
                for neighbor in neighbors {
                    if neighbor.get() >= node_count
                        || neighbor.get() == node_id
                        || !unique.insert(*neighbor)
                        || self.nodes[neighbor.get()].layers.len() <= layer_index
                    {
                        return Err(snapshot_error("neighbor reference is invalid"));
                    }
                }
            }
        }
        let Some(entry) = self.entry_point else {
            if self.nodes.is_empty() {
                return Ok(());
            }
            return Err(snapshot_error("nonempty graph has no entry point"));
        };
        if entry.get() >= node_count || self.node_level(entry) != self.max_level() {
            return Err(snapshot_error("entry point is invalid"));
        }
        for (node_id, node) in self.nodes.iter().enumerate() {
            for (layer_index, neighbors) in node.layers.iter().enumerate() {
                for neighbor in neighbors {
                    if !self.nodes[neighbor.get()].layers[layer_index]
                        .contains(&HnswNodeId::new(node_id))
                    {
                        return Err(snapshot_error("neighbor link is not reciprocal"));
                    }
                }
            }
        }
        let maximum_level = self.max_level().map_or(0, HnswLevel::get);
        for layer_index in 0..=maximum_level {
            let participants = self
                .nodes
                .iter()
                .enumerate()
                .filter_map(|(node_id, node)| {
                    (node.layers.len() > layer_index).then_some(HnswNodeId::new(node_id))
                })
                .collect::<BTreeSet<_>>();
            let Some(start) = participants.first().copied() else {
                return Err(snapshot_error("hierarchy layer has no participant"));
            };
            let mut reached = BTreeSet::new();
            let mut pending = VecDeque::from([start]);
            while let Some(node_id) = pending.pop_front() {
                if !reached.insert(node_id) {
                    continue;
                }
                pending.extend(
                    self.nodes[node_id.get()].layers[layer_index]
                        .iter()
                        .copied(),
                );
            }
            if reached != participants {
                return Err(snapshot_error("hierarchy layer is disconnected"));
            }
        }
        Ok(())
    }
}
