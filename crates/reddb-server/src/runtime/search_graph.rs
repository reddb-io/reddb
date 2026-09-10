//! Context-search graph expansion under the enclosing statement's read view.
use super::execution_context::capture_current_snapshot;
use super::function_budget;
use super::*;
use crate::storage::unified::segment::GraphEntityKind;
pub(super) mod memory;
use memory::GraphMemory;

struct SearchGraphNodeLocation {
    collection: String,
    physical_id: EntityId,
    aliases: Vec<EntityId>,
}

struct SearchGraphNeighbor {
    logical_id: String,
    edge_id: u64,
    collection: String,
    physical_id: EntityId,
}

/// Query-local caches contain only probed identities and reached adjacency.
struct SearchGraphRead<'a> {
    runtime: &'a RedDBRuntime,
    store: &'a UnifiedStore,
    snapshot: Option<super::impl_core::SnapshotContext>,
    collections: Vec<String>,
    nodes: HashMap<EntityId, Option<SearchGraphNodeLocation>>,
    endpoints: HashMap<String, Option<EntityId>>,
    adjacency: HashMap<String, Vec<SearchGraphNeighbor>>,
}

impl SearchGraphRead<'_> {
    fn visible(&self, entity: &UnifiedEntity) -> bool {
        // Preserve the scalar scan fallback for internal callers without a frame.
        (self.snapshot.is_some() || entity.xmax == 0)
            && super::impl_core::entity_visible_with_context(self.snapshot.as_ref(), entity)
    }

    fn node(
        &mut self,
        logical: EntityId,
        memory: &mut GraphMemory<'_>,
    ) -> RedDBResult<Option<&SearchGraphNodeLocation>> {
        if !self.nodes.contains_key(&logical) {
            memory.entry::<(EntityId, Option<SearchGraphNodeLocation>)>(0)?;
            memory.entry::<EntityId>(0)?;
            let mut aliases = vec![logical];
            let mut visible = None;
            for collection in &self.collections {
                function_budget::charge(1)?;
                let Some(manager) = self.store.get_collection(collection) else {
                    continue;
                };
                let mut cursor_memory = memory.scratch();
                manager.visit_graph_index_batches(
                    GraphEntityKind::Node,
                    &[logical],
                    || function_budget::charge(1).is_ok(),
                    |bytes| cursor_memory.admit(bytes),
                    |ids| {
                        for id in ids {
                            function_budget::charge(1)?;
                            let Some(is_visible) = manager
                                .get_with(*id, |entity| {
                                    (matches!(entity.kind, EntityKind::GraphNode(_))
                                        && entity.logical_id() == logical)
                                        .then(|| self.visible(entity))
                                })
                                .flatten()
                            else {
                                continue;
                            };
                            memory.entry::<EntityId>(0)?;
                            aliases.push(*id);
                            if is_visible {
                                memory.admit(collection.len())?;
                                visible = Some((collection.clone(), *id));
                            }
                        }
                        Ok(true)
                    },
                )?;
                function_budget::charge(0)?;
            }
            aliases.sort_unstable();
            aliases.dedup();
            self.nodes.insert(
                logical,
                visible.map(|(collection, physical_id)| SearchGraphNodeLocation {
                    collection,
                    physical_id,
                    aliases,
                }),
            );
        }
        Ok(self.nodes.get(&logical).and_then(Option::as_ref))
    }

    fn endpoint(
        &mut self,
        endpoint: &str,
        memory: &mut GraphMemory<'_>,
    ) -> RedDBResult<Option<EntityId>> {
        if let Some(identity) = self.endpoints.get(endpoint) {
            return Ok(*identity);
        }
        let mut resolved = None;
        if let Ok(id) = endpoint.parse::<u64>() {
            let id = EntityId::new(id);
            if self.node(id, memory)?.is_some() {
                resolved = Some(id);
            } else {
                // Retained physical aliases reveal identity only. A visible
                // logical version must still exist in the requested scope.
                let mut logical = None;
                for collection in &self.collections {
                    function_budget::charge(1)?;
                    let Some(manager) = self.store.get_collection(collection) else {
                        continue;
                    };
                    logical =
                        manager.graph_node_logical_id(id, || function_budget::charge(1).is_ok());
                    function_budget::charge(0)?;
                    if logical.is_some() {
                        break;
                    }
                }
                if let Some(logical) = logical {
                    if self.node(logical, memory)?.is_some() {
                        resolved = Some(logical);
                    }
                }
            }
        }
        memory.entry::<(String, Option<EntityId>)>(endpoint.len())?;
        self.endpoints.insert(endpoint.to_string(), resolved);
        Ok(resolved)
    }

    fn neighbors(
        &mut self,
        current: &str,
        max_edges: usize,
        hydrated: &mut HashMap<String, Option<UnifiedEntity>>,
        memory: &mut GraphMemory<'_>,
        policies: &mut HashMap<String, Option<reddb_rql::ast::Filter>>,
    ) -> RedDBResult<&[SearchGraphNeighbor]> {
        if !self.adjacency.contains_key(current) {
            // Candidates and deduplication live only until this node's ordered,
            // policy-admitted prefix has moved into the permanent cache.
            let mut candidates_memory = memory.scratch();
            let logical = EntityId::new(current.parse().expect("numeric logical node ID"));
            let aliases = if let Some(node) = self.node(logical, memory)? {
                candidates_memory.admit(std::mem::size_of_val(node.aliases.as_slice()))?;
                node.aliases.clone()
            } else {
                Vec::new()
            };
            let mut seen = HashSet::new();
            let mut neighbors = Vec::new();
            if !aliases.is_empty() {
                for index in 0..self.collections.len() {
                    function_budget::charge(1)?;
                    candidates_memory.admit(self.collections[index].len())?;
                    let collection = self.collections[index].clone();
                    let Some(manager) = self.store.get_collection(&collection) else {
                        continue;
                    };
                    let mut cursor_memory = memory.scratch();
                    manager.visit_graph_index_batches(
                        GraphEntityKind::Edge,
                        &aliases,
                        || function_budget::charge(1).is_ok(),
                        |bytes| cursor_memory.admit(bytes),
                        |ids| {
                            self.neighbors_batch(
                                logical,
                                &collection,
                                ids,
                                &mut seen,
                                &mut neighbors,
                                &mut candidates_memory,
                                memory,
                                policies,
                            )?;
                            Ok(true)
                        },
                    )?;
                    function_budget::charge(0)?;
                }
            }
            neighbors.sort_unstable_by(|left, right| {
                left.logical_id
                    .cmp(&right.logical_id)
                    .then_with(|| left.edge_id.cmp(&right.edge_id))
            });
            let neighbors =
                self.neighbors_admitted(neighbors, max_edges, hydrated, memory, policies)?;
            memory.entry::<(String, Vec<SearchGraphNeighbor>)>(current.len())?;
            self.adjacency.insert(current.to_string(), neighbors);
            // Drop temporary owners before their credit scope is refunded.
            drop((seen, aliases));
        }
        function_budget::charge(0)?;
        Ok(self.adjacency.get(current).expect("adjacency resolved"))
    }

    fn neighbors_admitted(
        &self,
        candidates: Vec<SearchGraphNeighbor>,
        max_edges: usize,
        hydrated: &mut HashMap<String, Option<UnifiedEntity>>,
        memory: &mut GraphMemory<'_>,
        policies: &mut HashMap<String, Option<reddb_rql::ast::Filter>>,
    ) -> RedDBResult<Vec<SearchGraphNeighbor>> {
        let mut admitted = Vec::new();
        for adjacent in candidates {
            function_budget::charge(1)?;
            if admitted.len() >= max_edges {
                break;
            }
            let neighbor = &adjacent.logical_id;
            if !hydrated.contains_key(neighbor) {
                memory.entry::<(String, Option<UnifiedEntity>)>(neighbor.len())?;
                let entity = memory
                    .entity(self.store, &adjacent.collection, adjacent.physical_id)?
                    .filter(|entity| {
                        self.runtime.search_entity_allowed(
                            &adjacent.collection,
                            entity,
                            self.snapshot.as_ref(),
                            policies,
                        )
                    });
                hydrated.insert(neighbor.clone(), entity);
            }
            if hydrated
                .get(neighbor)
                .expect("neighbor hydration resolved")
                .is_none()
            {
                continue;
            }
            // Selection is independent of each source's visited set. Parallel
            // edges and self-loops still consume the per-source edge allowance.
            memory.entry::<SearchGraphNeighbor>(neighbor.len() + adjacent.collection.len())?;
            admitted.push(adjacent);
        }
        Ok(admitted)
    }

    fn neighbors_batch(
        &mut self,
        logical: EntityId,
        collection: &str,
        ids: &[EntityId],
        seen: &mut HashSet<EntityId>,
        neighbors: &mut Vec<SearchGraphNeighbor>,
        candidates_memory: &mut GraphMemory<'_>,
        memory: &mut GraphMemory<'_>,
        policies: &mut HashMap<String, Option<reddb_rql::ast::Filter>>,
    ) -> RedDBResult<()> {
        let mut scratch = memory.scratch();
        for id in ids {
            function_budget::charge(1)?;
            if seen.contains(id) {
                continue;
            }
            let Some(entity) =
                scratch.entity_if(self.store, collection, *id, |entity| self.visible(entity))?
            else {
                continue;
            };
            candidates_memory.entry::<EntityId>(0)?;
            seen.insert(*id);
            if !self.runtime.search_entity_allowed(
                collection,
                &entity,
                self.snapshot.as_ref(),
                policies,
            ) {
                continue;
            }
            let EntityKind::GraphEdge(edge) = &entity.kind else {
                continue;
            };
            let Some(from) = self.endpoint(&edge.from_node, memory)? else {
                continue;
            };
            let Some(to) = self.endpoint(&edge.to_node, memory)? else {
                continue;
            };
            let neighbor = if from == logical {
                to
            } else if to == logical {
                from
            } else {
                continue;
            };
            let node = self
                .node(neighbor, memory)?
                .expect("endpoint has a visible node");
            candidates_memory.entry::<SearchGraphNeighbor>(20 + node.collection.len())?;
            neighbors.push(SearchGraphNeighbor {
                logical_id: neighbor.raw().to_string(),
                edge_id: entity.logical_id().raw(),
                collection: node.collection.clone(),
                physical_id: node.physical_id,
            });
        }
        Ok(())
    }
}

impl RedDBRuntime {
    pub(super) fn search_context_expand_graph(
        &self,
        scored: &mut HashMap<u64, (UnifiedEntity, f32, DiscoveryMethod, String)>,
        scope: Option<&BTreeSet<String>>,
        max_depth: usize,
        max_edges: usize,
        min_score: f32,
        memory: &mut GraphMemory<'_>,
        policies: &mut HashMap<String, Option<reddb_rql::ast::Filter>>,
    ) -> RedDBResult<usize> {
        let snapshot = capture_current_snapshot();
        // Retain these credits through result assembly in the caller. Input
        // payloads are owned by the preceding search stages, not cloned here.
        let graph_count = scored
            .values()
            .filter(|(entity, _, _, collection)| {
                matches!(entity.kind, EntityKind::GraphNode(_))
                    && scope.is_none_or(|scope| scope.contains(collection))
            })
            .count();
        if graph_count == 0 {
            function_budget::charge(0)?;
            return Ok(0);
        }
        memory.admit(graph_count.saturating_mul(256))?;
        let mut seeds: Vec<_> = scored
            .values()
            .filter(|(entity, _, _, collection)| {
                matches!(entity.kind, EntityKind::GraphNode(_))
                    && scope.is_none_or(|scope| scope.contains(collection))
                    && self.search_entity_allowed(collection, entity, snapshot.as_ref(), policies)
            })
            .map(|(entity, score, _, _)| (entity.logical_id().raw().to_string(), *score))
            .collect();
        function_budget::charge(0)?;
        if seeds.is_empty() {
            return Ok(0);
        }
        // Strongest source wins; stable identity resolves ties independently of hash order.
        seeds.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let store = self.db().store();
        let collections: Vec<_> = store
            .list_collections()
            .into_iter()
            .filter(|collection| scope.is_none_or(|scope| scope.contains(collection)))
            .collect();
        let mut graph = SearchGraphRead {
            runtime: self,
            store: store.as_ref(),
            snapshot: snapshot.clone(),
            collections,
            nodes: HashMap::new(),
            endpoints: HashMap::new(),
            adjacency: HashMap::new(),
        };
        let mut hydrated: HashMap<String, Option<UnifiedEntity>> = HashMap::new();
        let mut expanded = 0;
        // Map logical identity to the existing result slot without changing the
        // physical entity envelope used by the other context-search stages.
        memory.admit(
            scored
                .values()
                .filter(|(entity, _, _, _)| matches!(entity.kind, EntityKind::GraphNode(_)))
                .count()
                .saturating_mul(256),
        )?;
        let mut result_slots: HashMap<_, _> = scored
            .iter()
            .filter(|(_, (entity, _, _, _))| matches!(entity.kind, EntityKind::GraphNode(_)))
            .map(|(slot, (entity, _, _, _))| (entity.logical_id().raw().to_string(), *slot))
            .collect();
        for (source, source_score) in seeds {
            memory.entry::<(String, usize)>(source.len())?;
            memory.entry::<String>(source.len())?;
            let mut visited = HashSet::from([source.clone()]);
            let mut queue = VecDeque::from([(source.clone(), 0usize)]);
            while let Some((current, depth)) = queue.pop_front() {
                function_budget::charge(1)?;
                if depth >= max_depth {
                    continue;
                }
                let neighbors =
                    graph.neighbors(&current, max_edges, &mut hydrated, memory, policies)?;
                for adjacent in neighbors {
                    let neighbor = &adjacent.logical_id;
                    function_budget::charge(1)?;
                    let collection = &adjacent.collection;
                    let entity = hydrated
                        .get(neighbor)
                        .and_then(Option::as_ref)
                        .expect("cached adjacency has an authorized hydrated node");
                    if visited.contains(neighbor) {
                        continue;
                    }
                    memory.entry::<String>(neighbor.len())?;
                    visited.insert(neighbor.clone());
                    memory.entry::<(String, usize)>(neighbor.len())?;
                    // A direct match still acts as a bridge to deeper neighbors.
                    queue.push_back((neighbor.clone(), depth + 1));
                    let score = source_score * 0.7f32.powi((depth + 1) as i32);
                    if score < min_score {
                        continue;
                    }
                    memory.admit(8)?;
                    let discovery = DiscoveryMethod::GraphTraversal {
                        source_id: source
                            .parse()
                            .expect("graph source is a numeric logical ID"),
                        edge_type: "adjacent".to_string(),
                        depth: depth + 1,
                    };
                    if let Some(slot) = result_slots.get(neighbor) {
                        let existing = scored.get_mut(slot).expect("graph result slot exists");
                        if matches!(existing.2, DiscoveryMethod::GraphTraversal { .. })
                            && score > existing.1
                        {
                            existing.1 = score;
                            existing.2 = discovery;
                        }
                    } else {
                        memory.entry::<(String, u64)>(neighbor.len())?;
                        memory.entry::<(u64, UnifiedEntity, f32, DiscoveryMethod, String)>(
                            collection.len() + 8,
                        )?;
                        memory.admit(memory::payload_bytes(entity))?;
                        result_slots.insert(neighbor.clone(), entity.id.raw());
                        scored.insert(
                            entity.id.raw(),
                            (entity.clone(), score, discovery, collection.clone()),
                        );
                        expanded += 1;
                    }
                }
            }
        }
        function_budget::charge(0)?;
        Ok(expanded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::SearchContextInput;

    fn memory_fixture(
        degree: usize,
        payload_bytes: usize,
        sealed: bool,
    ) -> (RedDBRuntime, UnifiedEntity) {
        let runtime = RedDBRuntime::with_options(
            RedDBOptions::in_memory().with_memory_budget(64 * 1024 * 1024),
        )
        .expect("runtime");
        let store = runtime.db().store();
        store.get_or_create_collection("links");
        let seed =
            UnifiedEntity::graph_node(store.next_entity_id(), "seed", "Node", HashMap::new());
        store.insert("links", seed.clone()).expect("seed");
        for _ in 0..degree {
            let id = store.next_entity_id();
            store
                .insert(
                    "links",
                    UnifiedEntity::graph_node(
                        id,
                        "neighbor",
                        "Node",
                        HashMap::from([("body".into(), Value::text("x".repeat(payload_bytes)))]),
                    ),
                )
                .expect("node");
            store
                .insert(
                    "links",
                    UnifiedEntity::graph_edge(
                        store.next_entity_id(),
                        "step",
                        seed.id.raw().to_string(),
                        id.raw().to_string(),
                        1.0,
                        HashMap::new(),
                    ),
                )
                .expect("edge");
        }
        if sealed {
            store
                .get_collection("links")
                .expect("collection")
                .force_seal()
                .expect("seal");
        }
        (runtime, seed)
    }

    #[test]
    fn graph_memory_denies_high_degree_and_large_payload_then_releases_credits() {
        use crate::storage::memory_pools::MemoryPool;
        for sealed in [false, true] {
            for (degree, payload, allowance) in [(2048, 0, 16 * 1024), (1, 256 * 1024, 64 * 1024)] {
                let (runtime, seed) = memory_fixture(degree, payload, sealed);
                runtime.refresh_memory_accounting();
                let headroom = runtime.memory_budget().resolved_bytes
                    - runtime.memory_accounting().total_used_bytes();
                let held = runtime
                    .admit_non_evictable_growth(
                        MemoryPool::IndexMemory,
                        "competing writer",
                        headroom - allowance,
                    )
                    .expect("writer reservation");
                let make_scored = || {
                    HashMap::from([(
                        seed.id.raw(),
                        (
                            seed.clone(),
                            1.0,
                            DiscoveryMethod::GlobalScan,
                            "links".into(),
                        ),
                    )])
                };
                let mut scored = make_scored();
                let error = runtime
                    .search_context_expand_graph(
                        &mut scored,
                        None,
                        1,
                        1,
                        0.0,
                        &mut GraphMemory::new(&runtime),
                        &mut HashMap::new(),
                    )
                    .expect_err("graph must fit");
                assert!(error.to_string().contains("over budget"), "{error}");
                assert_eq!(
                    scored.len(),
                    1,
                    "preparation fails before publishing a neighbor"
                );
                let reclaimed = runtime
                    .admit_non_evictable_growth(
                        MemoryPool::IndexMemory,
                        "failed query released all credits",
                        allowance,
                    )
                    .expect("no leaked query credits");
                drop((reclaimed, held));
                let mut memory = GraphMemory::new(&runtime);
                let mut scored = make_scored();
                assert_eq!(
                    runtime
                        .search_context_expand_graph(
                            &mut scored,
                            None,
                            1,
                            1,
                            0.0,
                            &mut memory,
                            &mut HashMap::new()
                        )
                        .expect("retry"),
                    1
                );
                assert_eq!(scored.len(), 2, "edge allowance is unchanged");
                assert!(
                    runtime
                        .admit_non_evictable_growth(
                            MemoryPool::IndexMemory,
                            "results still owned",
                            headroom
                        )
                        .is_err(),
                    "query retains its reservation"
                );
                drop((scored, memory));
                let _returned = runtime
                    .admit_non_evictable_growth(MemoryPool::IndexMemory, "finished query", headroom)
                    .expect("query completion returns credits");
            }
        }
    }

    #[test]
    fn graph_batches_reuse_payload_headroom_across_high_degree() {
        use crate::storage::memory_pools::MemoryPool;
        for sealed in [false, true] {
            let (runtime, seed) = memory_fixture(0, 0, false);
            let store = runtime.db().store();
            let neighbor_id = store.next_entity_id();
            store
                .insert(
                    "links",
                    UnifiedEntity::graph_node(neighbor_id, "neighbor", "Node", HashMap::new()),
                )
                .expect("neighbor");
            let payload = Value::text("x".repeat(4096));
            let edges = (0..2048)
                .map(|_| {
                    UnifiedEntity::graph_edge(
                        store.next_entity_id(),
                        "step",
                        seed.id.raw().to_string(),
                        neighbor_id.raw().to_string(),
                        1.0,
                        HashMap::from([("body".into(), payload.clone())]),
                    )
                })
                .collect();
            store.bulk_insert("links", edges).expect("parallel edges");
            if sealed {
                store
                    .get_collection("links")
                    .expect("collection")
                    .force_seal()
                    .expect("seal");
            }
            runtime.refresh_memory_accounting();
            let headroom = runtime.memory_budget().resolved_bytes
                - runtime.memory_accounting().total_used_bytes();
            let held = runtime
                .admit_non_evictable_growth(
                    MemoryPool::IndexMemory,
                    "competing query",
                    headroom - 4 * 1024 * 1024,
                )
                .expect("leave bounded scratch");
            let mut scored = HashMap::from([(
                seed.id.raw(),
                (
                    seed.clone(),
                    1.0,
                    DiscoveryMethod::GlobalScan,
                    "links".into(),
                ),
            )]);
            let mut memory = GraphMemory::new(&runtime);
            assert_eq!(
                runtime
                    .search_context_expand_graph(
                        &mut scored,
                        None,
                        1,
                        1,
                        0.0,
                        &mut memory,
                        &mut HashMap::new()
                    )
                    .expect("batch payloads must reuse headroom"),
                1
            );
            assert_eq!(scored.len(), 2);
            assert!(scored.contains_key(&neighbor_id.raw()));
            drop((scored, memory, held));
            let _returned = runtime
                .admit_non_evictable_growth(
                    MemoryPool::IndexMemory,
                    "all query credits returned",
                    headroom,
                )
                .expect("no leaked batch credits");
        }
    }

    #[test]
    fn graph_cached_adjacency_preserves_authorized_edge_allowance() {
        for sealed in [false, true] {
            let (runtime, _) = memory_fixture(0, 0, false);
            let store = runtime.db().store();
            let mut seeds = HashMap::new();
            for (id, allowed) in [(2, true), (10, false), (11, true), (100, true), (200, true)] {
                let entity = UnifiedEntity::graph_node(
                    EntityId::new(id),
                    "node",
                    "Node",
                    HashMap::from([("allowed".into(), Value::Boolean(allowed))]),
                );
                store.insert("links", entity.clone()).expect("node");
                if id >= 100 {
                    seeds.insert(
                        id,
                        (entity, 1.0, DiscoveryMethod::GlobalScan, "links".into()),
                    );
                }
            }
            // Physical order differs from lexical logical order. The denied
            // endpoint sorts first, then the self-loop, two parallel edges,
            // and node 2 (which sorts after node 11).
            for (index, (from, to)) in [
                (100, 2),
                (100, 11),
                (100, 10),
                (100, 11),
                (100, 100),
                (200, 100),
            ]
            .into_iter()
            .enumerate()
            {
                store
                    .insert(
                        "links",
                        UnifiedEntity::graph_edge(
                            EntityId::new(1000 + u64::try_from(index).expect("small edge index")),
                            "step",
                            from.to_string(),
                            to.to_string(),
                            1.0,
                            HashMap::new(),
                        ),
                    )
                    .expect("edge");
            }
            for query in [
                "CREATE POLICY nodes ON NODES OF links USING (properties.allowed = true)",
                "CREATE POLICY edges ON EDGES OF links USING (true)",
                "ALTER TABLE links ENABLE ROW LEVEL SECURITY",
            ] {
                runtime.execute_query(query).expect("policy");
            }
            if sealed {
                store
                    .get_collection("links")
                    .expect("collection")
                    .force_seal()
                    .expect("seal");
            }
            for (max_edges, expected) in [
                (1, vec![100, 200]),
                (2, vec![11, 100, 200]),
                (3, vec![11, 100, 200]),
                (4, vec![2, 11, 100, 200]),
            ] {
                let mut scored = seeds.clone();
                runtime
                    .search_context_expand_graph(
                        &mut scored,
                        None,
                        2,
                        max_edges,
                        0.0,
                        &mut GraphMemory::new(&runtime),
                        &mut HashMap::new(),
                    )
                    .expect("ordered expansion with cached source revisits");
                let mut actual: Vec<_> = scored.keys().copied().collect();
                actual.sort_unstable();
                assert_eq!(
                    actual, expected,
                    "edge allowance {max_edges}, sealed {sealed}"
                );
                for id in [100, 200] {
                    assert!(matches!(scored[&id].2, DiscoveryMethod::GlobalScan));
                }
                if max_edges >= 2 {
                    assert_eq!(scored[&11].1, 0.7);
                }
            }
        }
    }

    #[test]
    fn graph_adjacency_reuses_headroom_across_high_degree_seeds() {
        use crate::storage::memory_pools::MemoryPool;
        for sealed in [false, true] {
            let (runtime, _) = memory_fixture(0, 0, false);
            let store = runtime.db().store();
            let mut scored = HashMap::new();
            let mut expected = Vec::new();
            for _ in 0..8 {
                let seed = UnifiedEntity::graph_node(
                    store.next_entity_id(),
                    "source",
                    "Node",
                    HashMap::new(),
                );
                let neighbor = UnifiedEntity::graph_node(
                    store.next_entity_id(),
                    "destination",
                    "Node",
                    HashMap::new(),
                );
                store.insert("links", seed.clone()).expect("source");
                store
                    .insert("links", neighbor.clone())
                    .expect("destination");
                let edges = (0..2048)
                    .map(|_| {
                        UnifiedEntity::graph_edge(
                            store.next_entity_id(),
                            "step",
                            seed.id.raw().to_string(),
                            neighbor.id.raw().to_string(),
                            1.0,
                            HashMap::new(),
                        )
                    })
                    .collect();
                store.bulk_insert("links", edges).expect("parallel edges");
                expected.push(neighbor.id.raw());
                scored.insert(
                    seed.id.raw(),
                    (seed, 1.0, DiscoveryMethod::GlobalScan, "links".into()),
                );
            }
            if sealed {
                store
                    .get_collection("links")
                    .expect("collection")
                    .force_seal()
                    .expect("seal");
            }
            runtime.refresh_memory_accounting();
            let headroom = runtime.memory_budget().resolved_bytes
                - runtime.memory_accounting().total_used_bytes();
            let held = runtime
                .admit_non_evictable_growth(
                    MemoryPool::IndexMemory,
                    "competing query",
                    headroom - 2 * 1024 * 1024,
                )
                .expect("leave adjacency scratch");
            let mut memory = GraphMemory::new(&runtime);
            assert_eq!(
                runtime
                    .search_context_expand_graph(
                        &mut scored,
                        None,
                        1,
                        1,
                        0.0,
                        &mut memory,
                        &mut HashMap::new(),
                    )
                    .expect("discarded adjacency must release query credits"),
                8
            );
            assert_eq!(scored.len(), 16);
            assert!(expected.iter().all(|id| scored.contains_key(id)));
            drop((scored, memory, held));
            let _returned = runtime
                .admit_non_evictable_growth(
                    MemoryPool::IndexMemory,
                    "all query credits returned",
                    headroom,
                )
                .expect("no leaked adjacency credits");
        }
    }

    #[test]
    fn graph_batch_scopes_share_small_query_headroom() {
        use crate::storage::memory_pools::MemoryPool;
        for allowance in [64 * 1024, 130 * 1024] {
            let (runtime, seed) = memory_fixture(1, 0, false);
            runtime.refresh_memory_accounting();
            let headroom = runtime.memory_budget().resolved_bytes
                - runtime.memory_accounting().total_used_bytes();
            let held = runtime
                .admit_non_evictable_growth(
                    MemoryPool::IndexMemory,
                    "other query",
                    headroom - allowance,
                )
                .expect("small query allowance");
            let mut scored = HashMap::from([(
                seed.id.raw(),
                (seed, 1.0, DiscoveryMethod::GlobalScan, "links".into()),
            )]);
            assert_eq!(
                runtime
                    .search_context_expand_graph(
                        &mut scored,
                        None,
                        1,
                        1,
                        0.0,
                        &mut GraphMemory::new(&runtime),
                        &mut HashMap::new()
                    )
                    .expect("nested scopes share credits"),
                1
            );
            drop((scored, held));
        }
    }

    #[test]
    fn graph_scratch_unwind_refunds_query_credits_without_releasing_active_scopes() {
        use crate::storage::memory_pools::MemoryPool;
        let (runtime, _) = memory_fixture(0, 0, false);
        runtime.refresh_memory_accounting();
        let headroom =
            runtime.memory_budget().resolved_bytes - runtime.memory_accounting().total_used_bytes();
        let held = runtime
            .admit_non_evictable_growth(
                MemoryPool::IndexMemory,
                "other operation",
                headroom - 64 * 1024,
            )
            .expect("query allowance");
        let mut memory = GraphMemory::new(&runtime);
        memory.admit(16 * 1024).expect("retained results");
        let mut active = memory.scratch();
        active.admit(16 * 1024).expect("live temporary buffer");
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut temporary = memory.scratch();
            temporary.admit(32 * 1024).expect("remaining credits");
            assert!(
                memory.admit(1).is_err(),
                "active temporary credits cannot be reused"
            );
            panic!("cancel temporary work");
        }));
        assert!(failed.is_err());
        memory
            .admit(32 * 1024)
            .expect("only unwound scope credits return");
        assert!(memory.admit(1).is_err(), "active scope remains charged");
        drop(active);
        memory
            .admit(16 * 1024)
            .expect("last scope returns its credits");
        drop((memory, held));
        let _returned = runtime
            .admit_non_evictable_growth(MemoryPool::IndexMemory, "finished query", headroom)
            .expect("runtime guard finally released");
    }

    #[test]
    fn graph_memory_error_reaches_public_context_search() {
        use crate::storage::memory_pools::MemoryPool;
        let (runtime, _) = memory_fixture(128, 0, false);
        let request = SearchContextInput {
            query: "seed".into(),
            field: None,
            vector: None,
            collections: Some(vec!["links".into()]),
            limit: Some(2),
            graph_depth: Some(1),
            graph_max_edges: Some(1),
            max_cross_refs: None,
            follow_cross_refs: Some(false),
            expand_graph: Some(true),
            global_scan: Some(true),
            reindex: Some(false),
            min_score: None,
        };
        runtime.refresh_memory_accounting();
        let headroom =
            runtime.memory_budget().resolved_bytes - runtime.memory_accounting().total_used_bytes();
        let held = runtime
            .admit_non_evictable_growth(
                MemoryPool::IndexMemory,
                "competing operation",
                headroom - 4096,
            )
            .expect("reserve");
        let error = runtime
            .search_context(request.clone())
            .expect_err("no partial public result");
        assert!(error.to_string().contains("over budget"), "{error}");
        drop(held);
        let result = runtime
            .search_context(request)
            .expect("retry public search");
        assert_eq!(result.graph.nodes.len(), 2);
        assert_eq!(result.summary.expanded_via_graph, 1);
    }

    #[test]
    fn graph_memory_admits_variable_identity_and_reference_payloads_before_clone() {
        use crate::storage::memory_pools::MemoryPool;
        use crate::storage::unified::entity::{CrossRef, RefType};
        for reference in [false, true] {
            let (runtime, _) = memory_fixture(0, 0, false);
            let store = runtime.db().store();
            let id = store.next_entity_id();
            let mut node = UnifiedEntity::graph_node(
                id,
                if reference {
                    "neighbor".into()
                } else {
                    "n".repeat(256 * 1024)
                },
                "Node",
                HashMap::new(),
            );
            if reference {
                node.add_cross_ref(CrossRef::new(
                    id,
                    id,
                    "c".repeat(256 * 1024),
                    RefType::RelatedTo,
                ));
            }
            store.insert("links", node).expect("payload");
            runtime.refresh_memory_accounting();
            let headroom = runtime.memory_budget().resolved_bytes
                - runtime.memory_accounting().total_used_bytes();
            let held = runtime
                .admit_non_evictable_growth(
                    MemoryPool::IndexMemory,
                    "other query",
                    headroom - 64 * 1024,
                )
                .expect("reserve");
            assert!(GraphMemory::new(&runtime)
                .entity(store.as_ref(), "links", id)
                .is_err());
            drop(held);
            assert!(GraphMemory::new(&runtime)
                .entity(store.as_ref(), "links", id)
                .expect("payload now fits")
                .is_some());
        }
    }

    #[test]
    fn concurrent_graph_queries_share_headroom_and_unwind_returns_it() {
        let (runtime, _) = memory_fixture(0, 0, false);
        runtime.refresh_memory_accounting();
        let headroom =
            runtime.memory_budget().resolved_bytes - runtime.memory_accounting().total_used_bytes();
        let barrier = std::sync::Barrier::new(2);
        let admitted = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..2 {
                let runtime = runtime.clone();
                let barrier = &barrier;
                let admitted = &admitted;
                scope.spawn(move || {
                    let mut memory = GraphMemory::new(&runtime);
                    barrier.wait();
                    if memory
                        .admit(usize::try_from(headroom).expect("headroom fits usize"))
                        .is_ok()
                    {
                        admitted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    barrier.wait();
                });
            }
        });
        assert_eq!(admitted.load(std::sync::atomic::Ordering::Relaxed), 1);
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut memory = GraphMemory::new(&runtime);
            memory
                .admit(usize::try_from(headroom).expect("headroom"))
                .expect("all credits reusable");
            panic!("query unwind");
        }));
        assert!(unwound.is_err());
        GraphMemory::new(&runtime)
            .admit(usize::try_from(headroom).expect("headroom"))
            .expect("unwind releases all credits");
    }

    #[test]
    fn expansion_propagates_budget_failure_and_scope_recovers() {
        let runtime = RedDBRuntime::in_memory().expect("runtime");
        let store = runtime.db().store();
        store.get_or_create_collection("links");
        let id = store.next_entity_id();
        let node = UnifiedEntity::graph_node(id, "seed", "Node", HashMap::new());
        store.insert("links", node.clone()).expect("seed");
        let mut scored = HashMap::from([(
            id.raw(),
            (node, 1.0, DiscoveryMethod::GlobalScan, "links".into()),
        )]);
        let mut policies = HashMap::new();
        {
            let _budget = function_budget::Scope::enter(1, 10_000).expect("budget");
            let error = runtime
                .search_context_expand_graph(
                    &mut scored,
                    None,
                    3,
                    20,
                    0.0,
                    &mut GraphMemory::new(&runtime),
                    &mut policies,
                )
                .expect_err("indexed expansion exceeds budget");
            assert!(
                error.to_string().contains("execution work_max exceeded"),
                "{error}"
            );
        }
        assert_eq!(
            runtime
                .search_context_expand_graph(
                    &mut scored,
                    None,
                    3,
                    20,
                    0.0,
                    &mut GraphMemory::new(&runtime),
                    &mut policies
                )
                .expect("scope restored"),
            0
        );
    }

    #[test]
    fn indexed_expansion_budget_ignores_unreachable_graph() {
        for sealed in [false, true] {
            let runtime = RedDBRuntime::in_memory().expect("runtime");
            let store = runtime.db().store();
            store.get_or_create_collection("links");
            let seed_id = store.next_entity_id();
            let seed = UnifiedEntity::graph_node(seed_id, "seed", "Node", HashMap::new());
            store.insert("links", seed.clone()).expect("seed");
            let neighbor_id = store.next_entity_id();
            store
                .insert(
                    "links",
                    UnifiedEntity::graph_node(neighbor_id, "neighbor", "Node", HashMap::new()),
                )
                .expect("neighbor");
            store
                .insert(
                    "links",
                    UnifiedEntity::graph_edge(
                        store.next_entity_id(),
                        "step",
                        seed_id.raw().to_string(),
                        neighbor_id.raw().to_string(),
                        1.0,
                        HashMap::new(),
                    ),
                )
                .expect("edge");
            let mut unrelated = Vec::new();
            for _ in 0..4096 {
                let id = store.next_entity_id();
                unrelated.push(UnifiedEntity::graph_node(
                    id,
                    "orphan",
                    "Node",
                    HashMap::new(),
                ));
                unrelated.push(UnifiedEntity::graph_edge(
                    store.next_entity_id(),
                    "isolated",
                    id.raw().to_string(),
                    id.raw().to_string(),
                    1.0,
                    HashMap::new(),
                ));
            }
            store
                .bulk_insert("links", unrelated)
                .expect("unreachable graph");
            if sealed {
                store
                    .get_collection("links")
                    .expect("collection")
                    .force_seal()
                    .expect("seal");
            }
            let mut scored = HashMap::from([(
                seed_id.raw(),
                (seed, 1.0, DiscoveryMethod::GlobalScan, "links".into()),
            )]);
            let scope = BTreeSet::from(["links".into()]);
            let _budget = function_budget::Scope::enter(128, 10_000).expect("budget");
            assert_eq!(
                runtime
                    .search_context_expand_graph(
                        &mut scored,
                        Some(&scope),
                        1,
                        2,
                        0.0,
                        &mut GraphMemory::new(&runtime),
                        &mut HashMap::new(),
                    )
                    .expect("unrelated entities do not consume candidate budget"),
                1
            );
            assert!(scored.contains_key(&neighbor_id.raw()));
        }
    }
}
