//! Context-search graph expansion under the enclosing statement's read view.
use super::execution_context::capture_current_snapshot;
use super::function_budget;
use super::graph_tvf::{resolve_materialized_graph_endpoint, visit_graph_materialization_entities};
use super::*;
use crate::storage::unified::segment::GraphEntityKind;

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
        // Retain only scalar locations. Payloads and RLS evaluation stay outside
        // segment locks and are loaded only when traversal reaches a candidate.
        let mut locations = HashMap::new();
        for collection in &collections {
            function_budget::charge(1)?;
            let Some(manager) = store.get_collection(collection) else {
                continue;
            };
            manager.scan_graph_candidates_for_each(
                snapshot.as_ref(),
                GraphEntityKind::Node,
                || function_budget::charge(1).is_ok(),
                |entity| {
                    if function_budget::charge(1).is_err() {
                        return false;
                    }
                    if matches!(entity.kind, EntityKind::GraphNode(_)) {
                        locations.insert(
                            entity.logical_id().raw().to_string(),
                            (collection.clone(), entity.id),
                        );
                    }
                    true
                },
            );
            function_budget::charge(0)?;
        }
        let visible_nodes: HashSet<_> = locations.keys().cloned().collect();
        let mut aliases = HashMap::new();
        let mut adjacency: HashMap<String, Vec<(String, u64)>> = HashMap::new();
        for collection in &collections {
            let Some(manager) = store.get_collection(collection) else {
                continue;
            };
            visit_graph_materialization_entities(
                &manager,
                snapshot.as_ref(),
                GraphEntityKind::Edge,
                |entity| {
                    if !self.search_entity_rls_allowed(collection, &entity, policies) {
                        return Ok(());
                    }
                    let EntityKind::GraphEdge(edge) = &entity.kind else {
                        return Ok(());
                    };
                    let Some(from) = resolve_materialized_graph_endpoint(
                        &edge.from_node,
                        &visible_nodes,
                        &mut aliases,
                        &store,
                        &collections,
                    )?
                    else {
                        return Ok(());
                    };
                    let Some(to) = resolve_materialized_graph_endpoint(
                        &edge.to_node,
                        &visible_nodes,
                        &mut aliases,
                        &store,
                        &collections,
                    )?
                    else {
                        return Ok(());
                    };
                    let edge_id = entity.logical_id().raw();
                    adjacency
                        .entry(from.to_string())
                        .or_default()
                        .push((to.to_string(), edge_id));
                    if from != to {
                        adjacency
                            .entry(to.to_string())
                            .or_default()
                            .push((from.to_string(), edge_id));
                    }
                    Ok(())
                },
            )?;
        }
        for neighbors in adjacency.values_mut() {
            neighbors.sort_unstable();
        }
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
                let Some(neighbors) = adjacency.get(&current) else {
                    continue;
                };
                let mut admitted_edges = 0;
                for (neighbor, _) in neighbors {
                    function_budget::charge(1)?;
                    if admitted_edges >= max_edges {
                        break;
                    }
                    let (collection, physical) = &locations[neighbor];
                    let entity = hydrated.entry(neighbor.clone()).or_insert_with(|| {
                        store.get(collection, *physical).filter(|entity| {
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
                .expect_err("scalar preparation exceeds budget");
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
}
