//! Context-search graph expansion under the enclosing statement's read view.
use super::execution_context::capture_current_snapshot;
use super::function_budget;
use super::*;
use crate::storage::unified::segment::GraphEntityKind;

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

    fn node(&mut self, logical: EntityId) -> RedDBResult<Option<&SearchGraphNodeLocation>> {
        if !self.nodes.contains_key(&logical) {
            let mut aliases = vec![logical];
            let mut visible = None;
            for collection in &self.collections {
                function_budget::charge(1)?;
                let Some(manager) = self.store.get_collection(collection) else {
                    continue;
                };
                manager.visit_graph_index_candidates(
                    GraphEntityKind::Node,
                    &[logical],
                    || function_budget::charge(1).is_ok(),
                    |entity| {
                        aliases.push(entity.id);
                        if self.visible(entity) {
                            visible = Some((collection.clone(), entity.id));
                        }
                        true
                    },
                );
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

    fn endpoint(&mut self, endpoint: &str) -> RedDBResult<Option<EntityId>> {
        if let Some(identity) = self.endpoints.get(endpoint) {
            return Ok(*identity);
        }
        let mut resolved = None;
        if let Ok(id) = endpoint.parse::<u64>() {
            let id = EntityId::new(id);
            if self.node(id)?.is_some() {
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
                    if self.node(logical)?.is_some() {
                        resolved = Some(logical);
                    }
                }
            }
        }
        self.endpoints.insert(endpoint.to_string(), resolved);
        Ok(resolved)
    }

    fn neighbors(
        &mut self,
        current: &str,
        policies: &mut HashMap<String, Option<reddb_rql::ast::Filter>>,
    ) -> RedDBResult<&[SearchGraphNeighbor]> {
        if !self.adjacency.contains_key(current) {
            let logical = EntityId::new(current.parse().expect("numeric logical node ID"));
            let aliases = self
                .node(logical)?
                .map(|node| node.aliases.clone())
                .unwrap_or_default();
            let mut candidates = Vec::new();
            let mut seen = HashSet::new();
            if !aliases.is_empty() {
                for collection in &self.collections {
                    function_budget::charge(1)?;
                    let Some(manager) = self.store.get_collection(collection) else {
                        continue;
                    };
                    manager.visit_graph_index_candidates(
                        GraphEntityKind::Edge,
                        &aliases,
                        || function_budget::charge(1).is_ok(),
                        |entity| {
                            if self.visible(entity) && seen.insert(entity.id) {
                                candidates.push((collection.clone(), entity.id));
                            }
                            true
                        },
                    );
                    function_budget::charge(0)?;
                }
            }
            let mut neighbors = Vec::new();
            // Hydrate and evaluate policies after releasing all segment locks.
            for (collection, id) in candidates {
                function_budget::charge(1)?;
                let Some(entity) = self.store.get(&collection, id) else {
                    continue;
                };
                if !self.runtime.search_entity_allowed(
                    &collection,
                    &entity,
                    self.snapshot.as_ref(),
                    policies,
                ) {
                    continue;
                }
                let EntityKind::GraphEdge(edge) = &entity.kind else {
                    continue;
                };
                let Some(from) = self.endpoint(&edge.from_node)? else {
                    continue;
                };
                let Some(to) = self.endpoint(&edge.to_node)? else {
                    continue;
                };
                let neighbor = if from == logical {
                    to
                } else if to == logical {
                    from
                } else {
                    continue;
                };
                let node = self.node(neighbor)?.expect("endpoint has a visible node");
                neighbors.push(SearchGraphNeighbor {
                    logical_id: neighbor.raw().to_string(),
                    edge_id: entity.logical_id().raw(),
                    collection: node.collection.clone(),
                    physical_id: node.physical_id,
                });
            }
            neighbors.sort_by(|left, right| {
                left.logical_id
                    .cmp(&right.logical_id)
                    .then_with(|| left.edge_id.cmp(&right.edge_id))
            });
            self.adjacency.insert(current.to_string(), neighbors);
        }
        function_budget::charge(0)?;
        Ok(self.adjacency.get(current).expect("adjacency resolved"))
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
        policies: &mut HashMap<String, Option<reddb_rql::ast::Filter>>,
    ) -> RedDBResult<usize> {
        let snapshot = capture_current_snapshot();
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
        seeds.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
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
        let mut result_slots: HashMap<_, _> = scored
            .iter()
            .filter(|(_, (entity, _, _, _))| matches!(entity.kind, EntityKind::GraphNode(_)))
            .map(|(slot, (entity, _, _, _))| (entity.logical_id().raw().to_string(), *slot))
            .collect();
        for (source, source_score) in seeds {
            let mut visited = HashSet::from([source.clone()]);
            let mut queue = VecDeque::from([(source.clone(), 0usize)]);
            while let Some((current, depth)) = queue.pop_front() {
                function_budget::charge(1)?;
                if depth >= max_depth {
                    continue;
                }
                let neighbors = graph.neighbors(&current, policies)?;
                let mut admitted_edges = 0;
                for adjacent in neighbors {
                    let neighbor = &adjacent.logical_id;
                    function_budget::charge(1)?;
                    if admitted_edges >= max_edges {
                        break;
                    }
                    let collection = &adjacent.collection;
                    let physical = adjacent.physical_id;
                    let entity = hydrated.entry(neighbor.clone()).or_insert_with(|| {
                        store.get(collection, physical).filter(|entity| {
                            self.search_entity_allowed(
                                collection,
                                entity,
                                snapshot.as_ref(),
                                policies,
                            )
                        })
                    });
                    let Some(entity) = entity else { continue };
                    admitted_edges += 1;
                    if !visited.insert(neighbor.clone()) {
                        continue;
                    }
                    // A direct match still acts as a bridge to deeper neighbors.
                    queue.push_back((neighbor.clone(), depth + 1));
                    let score = source_score * 0.7f32.powi((depth + 1) as i32);
                    if score < min_score {
                        continue;
                    }
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
                .search_context_expand_graph(&mut scored, None, 3, 20, 0.0, &mut policies)
                .expect_err("indexed expansion exceeds budget");
            assert!(
                error.to_string().contains("execution work_max exceeded"),
                "{error}"
            );
        }
        assert_eq!(
            runtime
                .search_context_expand_graph(&mut scored, None, 3, 20, 0.0, &mut policies)
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
                        &mut HashMap::new(),
                    )
                    .expect("unrelated entities do not consume candidate budget"),
                1
            );
            assert!(scored.contains_key(&neighbor_id.raw()));
        }
    }
}
