//! Tree management over graph collections.

use crate::application::entity::json_to_metadata_value;
use crate::application::ports::RuntimeEntityPort;
use crate::storage::query::ast::{
    CreateTreeQuery, DropTreeQuery, TreeCommand, TreeNodeSpec, TreePosition,
};
use crate::storage::unified::{Metadata, MetadataValue};

use super::*;

const TREE_CHILD_EDGE_LABEL: &str = "TREE_CHILD";
const TREE_METADATA_PREFIX: &str = "red.tree.";
const TREE_METADATA_NAME: &str = "red.tree.name";
const TREE_METADATA_MAX_CHILDREN: &str = "red.tree.max_children";
const TREE_METADATA_CHILD_INDEX: &str = "red.tree.child_index";
const TREE_OWNERSHIP_OWNED: &str = "owned";
const TREE_AUTO_FIX_CONSERVATIVE: &str = "conservative";

#[derive(Debug, Clone)]
struct TreeIssue {
    code: &'static str,
    message: String,
    entity_id: Option<EntityId>,
}

#[derive(Debug, Clone)]
struct TreeNodeState {
    metadata: Metadata,
    max_children_override: Option<usize>,
}

#[derive(Debug, Clone)]
struct TreeChildLink {
    edge_id: EntityId,
    parent_id: EntityId,
    child_id: EntityId,
    child_index: usize,
}

#[derive(Debug, Clone)]
struct TreeState {
    nodes: BTreeMap<EntityId, TreeNodeState>,
    structural_edges: BTreeMap<EntityId, TreeChildLink>,
    children_by_parent: BTreeMap<EntityId, Vec<TreeChildLink>>,
    parent_by_child: BTreeMap<EntityId, TreeChildLink>,
}

#[derive(Debug, Clone)]
struct TreeValidation {
    issues: Vec<TreeIssue>,
    depths: BTreeMap<EntityId, usize>,
}

#[derive(Debug, Clone)]
struct TreeRebalancePlanRow {
    node_id: EntityId,
    old_parent_id: EntityId,
    new_parent_id: EntityId,
    old_index: usize,
    new_index: usize,
    old_depth: usize,
    new_depth: usize,
    changed: bool,
}

impl RedDBRuntime {
    pub fn execute_create_tree(
        &self,
        raw_query: &str,
        query: &CreateTreeQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        if query.default_max_children == 0 {
            return Err(RedDBError::Query(
                "tree default max children must be positive".to_string(),
            ));
        }

        if self
            .inner
            .db
            .tree_definition(&query.collection, &query.name)
            .is_some()
        {
            if query.if_not_exists {
                return Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("tree '{}.{}' already exists", query.collection, query.name),
                    "create",
                ));
            }
            return Err(RedDBError::Query(format!(
                "tree '{}.{}' already exists",
                query.collection, query.name
            )));
        }

        let root = self.create_node_unchecked(self.build_tree_create_node_input(
            &query.collection,
            &query.name,
            &query.root,
            false,
        )?)?;
        self.ensure_tree_root_metadata(
            &query.collection,
            root.id,
            &query.name,
            query.root.max_children,
        )?;
        let now = current_tree_unix_ms();
        let definition = crate::physical::PhysicalTreeDefinition {
            collection: query.collection.clone(),
            name: query.name.clone(),
            root_id: root.id.raw(),
            default_max_children: query.default_max_children,
            ordered_children: true,
            ownership: TREE_OWNERSHIP_OWNED.to_string(),
            auto_fix_mode: TREE_AUTO_FIX_CONSERVATIVE.to_string(),
            created_at_unix_ms: now,
            updated_at_unix_ms: now,
        };
        self.inner
            .db
            .save_tree_definition(definition)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        self.inner
            .db
            .persist_metadata()
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        self.note_table_write(&query.collection);

        Ok(RuntimeQueryResult::ok_records(
            raw_query.to_string(),
            vec![
                "collection".to_string(),
                "tree_name".to_string(),
                "root_id".to_string(),
                "default_max_children".to_string(),
            ],
            vec![vec![
                (
                    "collection".to_string(),
                    Value::text(query.collection.clone()),
                ),
                ("tree_name".to_string(), Value::text(query.name.clone())),
                ("root_id".to_string(), Value::UnsignedInteger(root.id.raw())),
                (
                    "default_max_children".to_string(),
                    Value::UnsignedInteger(query.default_max_children as u64),
                ),
            ]],
            "create",
        ))
    }

    pub fn execute_drop_tree(
        &self,
        raw_query: &str,
        query: &DropTreeQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        let Some(definition) = self
            .inner
            .db
            .tree_definition(&query.collection, &query.name)
        else {
            if query.if_exists {
                return Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("tree '{}.{}' does not exist", query.collection, query.name),
                    "drop",
                ));
            }
            return Err(RedDBError::NotFound(format!(
                "tree '{}.{}' not found",
                query.collection, query.name
            )));
        };

        let state = self.load_tree_state(&definition)?;
        let node_ids: BTreeSet<EntityId> = state.nodes.keys().copied().collect();
        let edge_ids = self.graph_edges_touching_nodes(&definition.collection, &node_ids)?;

        for edge_id in edge_ids {
            let _ = self.delete_entity_internal(&definition.collection, edge_id)?;
        }

        let mut ordered_nodes: Vec<EntityId> = node_ids.into_iter().collect();
        ordered_nodes.sort_by(|left, right| right.cmp(left));
        for node_id in ordered_nodes {
            let _ = self.delete_entity_internal(&definition.collection, node_id)?;
        }

        self.inner
            .db
            .remove_tree_definition(&query.collection, &query.name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        self.inner
            .db
            .persist_metadata()
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        self.note_table_write(&query.collection);

        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &format!("tree '{}.{}' dropped", query.collection, query.name),
            "drop",
        ))
    }

    pub fn execute_tree_command(
        &self,
        raw_query: &str,
        command: &TreeCommand,
    ) -> RedDBResult<RuntimeQueryResult> {
        match command {
            TreeCommand::Insert {
                collection,
                tree_name,
                parent_id,
                node,
                position,
            } => self.execute_tree_insert(
                raw_query,
                collection,
                tree_name,
                EntityId::new(*parent_id),
                node,
                *position,
            ),
            TreeCommand::Move {
                collection,
                tree_name,
                node_id,
                parent_id,
                position,
            } => self.execute_tree_move(
                raw_query,
                collection,
                tree_name,
                EntityId::new(*node_id),
                EntityId::new(*parent_id),
                *position,
            ),
            TreeCommand::Delete {
                collection,
                tree_name,
                node_id,
            } => {
                self.execute_tree_delete(raw_query, collection, tree_name, EntityId::new(*node_id))
            }
            TreeCommand::Validate {
                collection,
                tree_name,
            } => self.execute_tree_validate(raw_query, collection, tree_name),
            TreeCommand::Rebalance {
                collection,
                tree_name,
                dry_run,
            } => self.execute_tree_rebalance(raw_query, collection, tree_name, *dry_run),
        }
    }

    fn execute_tree_insert(
        &self,
        raw_query: &str,
        collection: &str,
        tree_name: &str,
        parent_id: EntityId,
        node: &TreeNodeSpec,
        position: TreePosition,
    ) -> RedDBResult<RuntimeQueryResult> {
        let definition = self.require_tree_definition(collection, tree_name)?;
        let state = self.load_tree_state(&definition)?;
        self.ensure_tree_operable(&definition, &state)?;

        if !state.nodes.contains_key(&parent_id) {
            return Err(RedDBError::NotFound(format!(
                "parent node '{}' was not found in tree '{}.{}'",
                parent_id.raw(),
                collection,
                tree_name
            )));
        }

        let mut children = child_id_list(&state.children_by_parent, parent_id);
        let insert_index = resolve_tree_insert_position(position, children.len())?;
        let parent_limit = effective_max_children(&definition, &state, parent_id)?;
        if children.len() >= parent_limit {
            return Err(RedDBError::Query(format!(
                "parent node '{}' is at capacity ({}) in tree '{}.{}'",
                parent_id.raw(),
                parent_limit,
                collection,
                tree_name
            )));
        }

        let created = self.create_node_unchecked(
            self.build_tree_create_node_input(collection, tree_name, node, true)?,
        )?;
        children.insert(insert_index, created.id);
        self.rewrite_parent_children(collection, tree_name, parent_id, &children, &state)?;
        self.touch_tree_definition_timestamp(&definition)?;
        self.note_table_write(collection);

        Ok(RuntimeQueryResult::ok_records(
            raw_query.to_string(),
            vec![
                "collection".to_string(),
                "tree_name".to_string(),
                "node_id".to_string(),
                "parent_id".to_string(),
                "child_index".to_string(),
            ],
            vec![vec![
                (
                    "collection".to_string(),
                    Value::text(collection.to_string()),
                ),
                ("tree_name".to_string(), Value::text(tree_name.to_string())),
                (
                    "node_id".to_string(),
                    Value::UnsignedInteger(created.id.raw()),
                ),
                (
                    "parent_id".to_string(),
                    Value::UnsignedInteger(parent_id.raw()),
                ),
                (
                    "child_index".to_string(),
                    Value::UnsignedInteger(insert_index as u64),
                ),
            ]],
            "insert",
        ))
    }

    fn execute_tree_move(
        &self,
        raw_query: &str,
        collection: &str,
        tree_name: &str,
        node_id: EntityId,
        new_parent_id: EntityId,
        position: TreePosition,
    ) -> RedDBResult<RuntimeQueryResult> {
        let definition = self.require_tree_definition(collection, tree_name)?;
        let state = self.load_tree_state(&definition)?;
        self.ensure_tree_operable(&definition, &state)?;

        if node_id.raw() == definition.root_id {
            return Err(RedDBError::Query("cannot move the tree root".to_string()));
        }
        if !state.nodes.contains_key(&node_id) {
            return Err(RedDBError::NotFound(format!(
                "node '{}' was not found in tree '{}.{}'",
                node_id.raw(),
                collection,
                tree_name
            )));
        }
        if !state.nodes.contains_key(&new_parent_id) {
            return Err(RedDBError::NotFound(format!(
                "parent node '{}' was not found in tree '{}.{}'",
                new_parent_id.raw(),
                collection,
                tree_name
            )));
        }
        if node_id == new_parent_id {
            return Err(RedDBError::Query(
                "node cannot be moved under itself".to_string(),
            ));
        }
        if subtree_ids(&state, node_id).contains(&new_parent_id) {
            return Err(RedDBError::Query(
                "node cannot be moved under one of its descendants".to_string(),
            ));
        }

        let current_parent = state
            .parent_by_child
            .get(&node_id)
            .map(|link| link.parent_id)
            .ok_or_else(|| {
                RedDBError::Query(format!(
                    "node '{}' does not have a structural parent in tree '{}.{}'",
                    node_id.raw(),
                    collection,
                    tree_name
                ))
            })?;

        let mut old_parent_children = child_id_list(&state.children_by_parent, current_parent);
        old_parent_children.retain(|child| *child != node_id);

        let mut new_parent_children = if current_parent == new_parent_id {
            old_parent_children.clone()
        } else {
            child_id_list(&state.children_by_parent, new_parent_id)
        };

        let insert_index = resolve_tree_insert_position(position, new_parent_children.len())?;
        let new_parent_limit = effective_max_children(&definition, &state, new_parent_id)?;
        if current_parent != new_parent_id && new_parent_children.len() >= new_parent_limit {
            return Err(RedDBError::Query(format!(
                "parent node '{}' is at capacity ({}) in tree '{}.{}'",
                new_parent_id.raw(),
                new_parent_limit,
                collection,
                tree_name
            )));
        }

        new_parent_children.insert(insert_index, node_id);

        if current_parent == new_parent_id {
            self.rewrite_parent_children(
                collection,
                tree_name,
                current_parent,
                &new_parent_children,
                &state,
            )?;
        } else {
            self.rewrite_parent_children(
                collection,
                tree_name,
                current_parent,
                &old_parent_children,
                &state,
            )?;
            self.rewrite_parent_children(
                collection,
                tree_name,
                new_parent_id,
                &new_parent_children,
                &state,
            )?;
        }

        self.touch_tree_definition_timestamp(&definition)?;
        self.note_table_write(collection);

        Ok(RuntimeQueryResult::ok_records(
            raw_query.to_string(),
            vec![
                "collection".to_string(),
                "tree_name".to_string(),
                "node_id".to_string(),
                "old_parent_id".to_string(),
                "new_parent_id".to_string(),
                "child_index".to_string(),
            ],
            vec![vec![
                (
                    "collection".to_string(),
                    Value::text(collection.to_string()),
                ),
                ("tree_name".to_string(), Value::text(tree_name.to_string())),
                ("node_id".to_string(), Value::UnsignedInteger(node_id.raw())),
                (
                    "old_parent_id".to_string(),
                    Value::UnsignedInteger(current_parent.raw()),
                ),
                (
                    "new_parent_id".to_string(),
                    Value::UnsignedInteger(new_parent_id.raw()),
                ),
                (
                    "child_index".to_string(),
                    Value::UnsignedInteger(insert_index as u64),
                ),
            ]],
            "update",
        ))
    }

    fn execute_tree_delete(
        &self,
        raw_query: &str,
        collection: &str,
        tree_name: &str,
        node_id: EntityId,
    ) -> RedDBResult<RuntimeQueryResult> {
        let definition = self.require_tree_definition(collection, tree_name)?;
        let state = self.load_tree_state(&definition)?;
        self.ensure_tree_operable(&definition, &state)?;

        if !state.nodes.contains_key(&node_id) {
            return Err(RedDBError::NotFound(format!(
                "node '{}' was not found in tree '{}.{}'",
                node_id.raw(),
                collection,
                tree_name
            )));
        }

        if node_id.raw() == definition.root_id {
            return self.execute_drop_tree(
                raw_query,
                &DropTreeQuery {
                    collection: collection.to_string(),
                    name: tree_name.to_string(),
                    if_exists: false,
                },
            );
        }

        let subtree = subtree_ids(&state, node_id);
        let parent_id = state
            .parent_by_child
            .get(&node_id)
            .map(|link| link.parent_id)
            .ok_or_else(|| {
                RedDBError::Query(format!(
                    "node '{}' does not have a structural parent in tree '{}.{}'",
                    node_id.raw(),
                    collection,
                    tree_name
                ))
            })?;
        let surviving_children: Vec<EntityId> = child_id_list(&state.children_by_parent, parent_id)
            .into_iter()
            .filter(|child| *child != node_id)
            .collect();

        let edge_ids = self.graph_edges_touching_nodes(collection, &subtree)?;
        for edge_id in edge_ids {
            let _ = self.delete_entity_internal(collection, edge_id)?;
        }

        let mut ordered_nodes: Vec<EntityId> = subtree.iter().copied().collect();
        ordered_nodes.sort_by(|left, right| right.cmp(left));
        for subtree_node in ordered_nodes {
            let _ = self.delete_entity_internal(collection, subtree_node)?;
        }

        self.rewrite_parent_children(
            collection,
            tree_name,
            parent_id,
            &surviving_children,
            &state,
        )?;
        self.touch_tree_definition_timestamp(&definition)?;
        self.note_table_write(collection);

        Ok(RuntimeQueryResult::ok_records(
            raw_query.to_string(),
            vec![
                "collection".to_string(),
                "tree_name".to_string(),
                "deleted_root_id".to_string(),
                "deleted_node_count".to_string(),
            ],
            vec![vec![
                (
                    "collection".to_string(),
                    Value::text(collection.to_string()),
                ),
                ("tree_name".to_string(), Value::text(tree_name.to_string())),
                (
                    "deleted_root_id".to_string(),
                    Value::UnsignedInteger(node_id.raw()),
                ),
                (
                    "deleted_node_count".to_string(),
                    Value::UnsignedInteger(subtree.len() as u64),
                ),
            ]],
            "delete",
        ))
    }

    fn execute_tree_validate(
        &self,
        raw_query: &str,
        collection: &str,
        tree_name: &str,
    ) -> RedDBResult<RuntimeQueryResult> {
        let definition = self.require_tree_definition(collection, tree_name)?;
        let state = self.load_tree_state(&definition)?;
        let validation = self.validate_tree_state(&definition, &state);

        if validation.issues.is_empty() {
            return Ok(RuntimeQueryResult::ok_records(
                raw_query.to_string(),
                vec![
                    "ok".to_string(),
                    "code".to_string(),
                    "message".to_string(),
                    "entity_id".to_string(),
                ],
                vec![vec![
                    ("ok".to_string(), Value::Boolean(true)),
                    ("code".to_string(), Value::text("ok".to_string())),
                    (
                        "message".to_string(),
                        Value::text("tree is valid".to_string()),
                    ),
                    ("entity_id".to_string(), Value::Null),
                ]],
                "select",
            ));
        }

        let rows = validation
            .issues
            .iter()
            .map(|issue| {
                vec![
                    ("ok".to_string(), Value::Boolean(false)),
                    ("code".to_string(), Value::text(issue.code.to_string())),
                    ("message".to_string(), Value::text(issue.message.clone())),
                    (
                        "entity_id".to_string(),
                        issue
                            .entity_id
                            .map(|entity_id| Value::UnsignedInteger(entity_id.raw()))
                            .unwrap_or(Value::Null),
                    ),
                ]
            })
            .collect();
        Ok(RuntimeQueryResult::ok_records(
            raw_query.to_string(),
            vec![
                "ok".to_string(),
                "code".to_string(),
                "message".to_string(),
                "entity_id".to_string(),
            ],
            rows,
            "select",
        ))
    }

    fn execute_tree_rebalance(
        &self,
        raw_query: &str,
        collection: &str,
        tree_name: &str,
        dry_run: bool,
    ) -> RedDBResult<RuntimeQueryResult> {
        let definition = self.require_tree_definition(collection, tree_name)?;
        let state = self.load_tree_state(&definition)?;
        self.ensure_tree_operable(&definition, &state)?;
        let plan = self.plan_tree_rebalance(&definition, &state)?;

        if !dry_run {
            let existing_edges: Vec<EntityId> = state.structural_edges.keys().copied().collect();
            for edge_id in existing_edges {
                let _ = self.delete_entity_internal(collection, edge_id)?;
            }

            for row in &plan {
                self.create_edge_unchecked(self.build_tree_structural_edge_input(
                    collection,
                    tree_name,
                    row.new_parent_id,
                    row.node_id,
                    row.new_index,
                ))?;
            }

            self.touch_tree_definition_timestamp(&definition)?;
            self.note_table_write(collection);
        }

        let rows = plan
            .iter()
            .map(|row| {
                vec![
                    (
                        "node_id".to_string(),
                        Value::UnsignedInteger(row.node_id.raw()),
                    ),
                    (
                        "old_parent_id".to_string(),
                        Value::UnsignedInteger(row.old_parent_id.raw()),
                    ),
                    (
                        "new_parent_id".to_string(),
                        Value::UnsignedInteger(row.new_parent_id.raw()),
                    ),
                    (
                        "old_index".to_string(),
                        Value::UnsignedInteger(row.old_index as u64),
                    ),
                    (
                        "new_index".to_string(),
                        Value::UnsignedInteger(row.new_index as u64),
                    ),
                    (
                        "old_depth".to_string(),
                        Value::UnsignedInteger(row.old_depth as u64),
                    ),
                    (
                        "new_depth".to_string(),
                        Value::UnsignedInteger(row.new_depth as u64),
                    ),
                    ("changed".to_string(), Value::Boolean(row.changed)),
                ]
            })
            .collect();

        Ok(RuntimeQueryResult::ok_records(
            raw_query.to_string(),
            vec![
                "node_id".to_string(),
                "old_parent_id".to_string(),
                "new_parent_id".to_string(),
                "old_index".to_string(),
                "new_index".to_string(),
                "old_depth".to_string(),
                "new_depth".to_string(),
                "changed".to_string(),
            ],
            rows,
            "select",
        ))
    }

    fn build_tree_create_node_input(
        &self,
        collection: &str,
        tree_name: &str,
        node: &TreeNodeSpec,
        allow_max_children: bool,
    ) -> RedDBResult<crate::application::CreateNodeInput> {
        if allow_max_children && node.max_children == Some(0) {
            return Err(RedDBError::Query(
                "node max children must be positive".to_string(),
            ));
        }
        self.ensure_tree_spec_is_user_safe(node)?;
        let mut metadata = build_tree_node_metadata(tree_name, &node.metadata)?;
        if allow_max_children {
            if let Some(max_children) = node.max_children {
                metadata.push((
                    TREE_METADATA_MAX_CHILDREN.to_string(),
                    MetadataValue::Int(max_children as i64),
                ));
            }
        }
        Ok(crate::application::CreateNodeInput {
            collection: collection.to_string(),
            label: node.label.clone(),
            node_type: node.node_type.clone(),
            properties: node.properties.clone(),
            metadata,
            embeddings: Vec::new(),
            table_links: Vec::new(),
            node_links: Vec::new(),
        })
    }

    fn build_tree_structural_edge_input(
        &self,
        collection: &str,
        tree_name: &str,
        parent_id: EntityId,
        child_id: EntityId,
        child_index: usize,
    ) -> crate::application::CreateEdgeInput {
        crate::application::CreateEdgeInput {
            collection: collection.to_string(),
            label: TREE_CHILD_EDGE_LABEL.to_string(),
            from: parent_id,
            to: child_id,
            weight: Some(1.0),
            properties: Vec::new(),
            metadata: vec![
                (
                    TREE_METADATA_NAME.to_string(),
                    MetadataValue::String(tree_name.to_string()),
                ),
                (
                    TREE_METADATA_CHILD_INDEX.to_string(),
                    MetadataValue::Int(child_index as i64),
                ),
            ],
        }
    }

    fn ensure_tree_spec_is_user_safe(&self, node: &TreeNodeSpec) -> RedDBResult<()> {
        for (key, _) in &node.metadata {
            if key.starts_with(TREE_METADATA_PREFIX) {
                return Err(RedDBError::Query(format!(
                    "metadata key '{}' is reserved for managed trees",
                    key
                )));
            }
        }
        Ok(())
    }

    fn require_tree_definition(
        &self,
        collection: &str,
        tree_name: &str,
    ) -> RedDBResult<crate::physical::PhysicalTreeDefinition> {
        self.inner
            .db
            .tree_definition(collection, tree_name)
            .ok_or_else(|| {
                RedDBError::NotFound(format!("tree '{}.{}' not found", collection, tree_name))
            })
    }

    fn touch_tree_definition_timestamp(
        &self,
        definition: &crate::physical::PhysicalTreeDefinition,
    ) -> RedDBResult<()> {
        let mut updated = definition.clone();
        updated.updated_at_unix_ms = current_tree_unix_ms();
        self.inner
            .db
            .save_tree_definition(updated)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        self.inner
            .db
            .persist_metadata()
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        Ok(())
    }

    fn ensure_tree_root_metadata(
        &self,
        collection: &str,
        root_id: EntityId,
        tree_name: &str,
        max_children_override: Option<usize>,
    ) -> RedDBResult<()> {
        let store = self.inner.db.store();
        let mut metadata = store.get_metadata(collection, root_id).unwrap_or_default();
        metadata.set(
            TREE_METADATA_NAME.to_string(),
            MetadataValue::String(tree_name.to_string()),
        );
        match max_children_override {
            Some(value) => metadata.set(
                TREE_METADATA_MAX_CHILDREN.to_string(),
                MetadataValue::Int(value as i64),
            ),
            None => {
                metadata.remove(TREE_METADATA_MAX_CHILDREN);
            }
        }
        store
            .set_metadata(collection, root_id, metadata)
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    fn load_tree_state(
        &self,
        definition: &crate::physical::PhysicalTreeDefinition,
    ) -> RedDBResult<TreeState> {
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection(&definition.collection) else {
            return Ok(TreeState {
                nodes: BTreeMap::new(),
                structural_edges: BTreeMap::new(),
                children_by_parent: BTreeMap::new(),
                parent_by_child: BTreeMap::new(),
            });
        };

        let mut all_nodes = BTreeMap::new();
        let mut candidate_links = Vec::new();
        let mut member_node_ids = BTreeSet::new();
        let root_id = EntityId::new(definition.root_id);

        for entity in manager.query_all(|entity| {
            matches!(
                entity.kind,
                EntityKind::GraphNode(_) | EntityKind::GraphEdge(_)
            )
        }) {
            let metadata = store
                .get_metadata(&definition.collection, entity.id)
                .unwrap_or_default();
            match &entity.kind {
                EntityKind::GraphNode(_) => {
                    if metadata_tree_name(&metadata) == Some(definition.name.as_str()) {
                        member_node_ids.insert(entity.id);
                    }
                    all_nodes.insert(
                        entity.id,
                        TreeNodeState {
                            max_children_override: metadata_usize(
                                &metadata,
                                TREE_METADATA_MAX_CHILDREN,
                            ),
                            metadata,
                        },
                    );
                }
                EntityKind::GraphEdge(edge_kind) if edge_kind.label == TREE_CHILD_EDGE_LABEL => {
                    let Some(parent_id) = parse_tree_entity_id_text(&edge_kind.from_node) else {
                        continue;
                    };
                    let Some(child_id) = parse_tree_entity_id_text(&edge_kind.to_node) else {
                        continue;
                    };
                    if metadata_tree_name(&metadata) == Some(definition.name.as_str()) {
                        member_node_ids.insert(parent_id);
                        member_node_ids.insert(child_id);
                    }
                    let child_index =
                        metadata_usize(&metadata, TREE_METADATA_CHILD_INDEX).unwrap_or(usize::MAX);
                    candidate_links.push(TreeChildLink {
                        edge_id: entity.id,
                        parent_id,
                        child_id,
                        child_index,
                    });
                }
                _ => {}
            }
        }

        let mut candidate_children_by_parent: BTreeMap<EntityId, Vec<TreeChildLink>> =
            BTreeMap::new();
        for link in &candidate_links {
            candidate_children_by_parent
                .entry(link.parent_id)
                .or_default()
                .push(link.clone());
        }

        for links in candidate_children_by_parent.values_mut() {
            links.sort_by(|left, right| {
                left.child_index
                    .cmp(&right.child_index)
                    .then_with(|| left.edge_id.cmp(&right.edge_id))
                    .then_with(|| left.child_id.cmp(&right.child_id))
            });
            for (derived_index, link) in links.iter_mut().enumerate() {
                link.child_index = derived_index;
            }
        }

        let mut queue = VecDeque::new();
        queue.push_back(root_id);
        while let Some(node_id) = queue.pop_front() {
            if !member_node_ids.insert(node_id) {
                continue;
            }
            if let Some(children) = candidate_children_by_parent.get(&node_id) {
                for child in children {
                    member_node_ids.insert(child.parent_id);
                    member_node_ids.insert(child.child_id);
                    queue.push_back(child.child_id);
                }
            }
        }

        let mut nodes = BTreeMap::new();
        for node_id in &member_node_ids {
            if let Some(node) = all_nodes.get(node_id) {
                nodes.insert(*node_id, node.clone());
            }
        }

        let mut structural_edges = BTreeMap::new();
        let mut children_by_parent: BTreeMap<EntityId, Vec<TreeChildLink>> = BTreeMap::new();
        let mut parent_by_child = BTreeMap::new();
        for links in candidate_children_by_parent.values() {
            for link in links {
                if !(member_node_ids.contains(&link.parent_id)
                    && member_node_ids.contains(&link.child_id))
                {
                    continue;
                }
                structural_edges.insert(link.edge_id, link.clone());
                children_by_parent
                    .entry(link.parent_id)
                    .or_default()
                    .push(link.clone());
                parent_by_child.insert(link.child_id, link.clone());
            }
        }

        for links in children_by_parent.values_mut() {
            links.sort_by(|left, right| {
                left.child_index
                    .cmp(&right.child_index)
                    .then_with(|| left.child_id.cmp(&right.child_id))
            });
        }

        Ok(TreeState {
            nodes,
            structural_edges,
            children_by_parent,
            parent_by_child,
        })
    }

    fn validate_tree_state(
        &self,
        definition: &crate::physical::PhysicalTreeDefinition,
        state: &TreeState,
    ) -> TreeValidation {
        let mut issues = Vec::new();
        let root_id = EntityId::new(definition.root_id);
        if !state.nodes.contains_key(&root_id) {
            issues.push(TreeIssue {
                code: "missing_root",
                message: format!(
                    "root node '{}' is missing from tree '{}.{}'",
                    definition.root_id, definition.collection, definition.name
                ),
                entity_id: Some(root_id),
            });
        }

        for (node_id, node) in &state.nodes {
            match metadata_tree_name(&node.metadata) {
                Some(name) if name == definition.name => {}
                Some(_) => issues.push(TreeIssue {
                    code: "node_tree_name_mismatch",
                    message: format!(
                        "node '{}' is part of tree '{}.{}' but is missing '{}' metadata",
                        node_id.raw(),
                        definition.collection,
                        definition.name,
                        TREE_METADATA_NAME
                    ),
                    entity_id: Some(*node_id),
                }),
                None => {}
            }
            if metadata_invalid_max_children(&node.metadata) {
                issues.push(TreeIssue {
                    code: "invalid_max_children",
                    message: format!(
                        "node '{}' has invalid '{}' metadata",
                        node_id.raw(),
                        TREE_METADATA_MAX_CHILDREN
                    ),
                    entity_id: Some(*node_id),
                });
            }
        }

        let mut parent_counts: BTreeMap<EntityId, usize> = BTreeMap::new();
        for link in state.structural_edges.values() {
            parent_counts
                .entry(link.child_id)
                .and_modify(|count| *count += 1)
                .or_insert(1);
            if !state.nodes.contains_key(&link.parent_id) {
                issues.push(TreeIssue {
                    code: "missing_parent_node",
                    message: format!(
                        "structural edge '{}' references missing parent node '{}'",
                        link.edge_id.raw(),
                        link.parent_id.raw()
                    ),
                    entity_id: Some(link.edge_id),
                });
            }
            if !state.nodes.contains_key(&link.child_id) {
                issues.push(TreeIssue {
                    code: "missing_child_node",
                    message: format!(
                        "structural edge '{}' references missing child node '{}'",
                        link.edge_id.raw(),
                        link.child_id.raw()
                    ),
                    entity_id: Some(link.edge_id),
                });
            }
        }

        for (child_id, count) in parent_counts {
            if count > 1 {
                issues.push(TreeIssue {
                    code: "multiple_parents",
                    message: format!("node '{}' has multiple structural parents", child_id.raw()),
                    entity_id: Some(child_id),
                });
            }
        }

        if let Some(link) = state.parent_by_child.get(&root_id) {
            issues.push(TreeIssue {
                code: "root_has_parent",
                message: format!(
                    "root node '{}' has a structural parent '{}'",
                    root_id.raw(),
                    link.parent_id.raw()
                ),
                entity_id: Some(root_id),
            });
        }

        for node_id in state.nodes.keys() {
            if *node_id == root_id {
                continue;
            }
            if !state.parent_by_child.contains_key(node_id) {
                issues.push(TreeIssue {
                    code: "missing_parent",
                    message: format!(
                        "node '{}' is disconnected from root '{}'",
                        node_id.raw(),
                        root_id.raw()
                    ),
                    entity_id: Some(*node_id),
                });
            }
        }

        for (parent_id, links) in &state.children_by_parent {
            let mut seen = BTreeSet::new();
            for (expected_index, link) in links.iter().enumerate() {
                if !seen.insert(link.child_index) {
                    issues.push(TreeIssue {
                        code: "duplicate_child_index",
                        message: format!(
                            "parent '{}' has duplicate child index '{}'",
                            parent_id.raw(),
                            link.child_index
                        ),
                        entity_id: Some(link.edge_id),
                    });
                }
                if link.child_index != expected_index {
                    issues.push(TreeIssue {
                        code: "non_compact_child_index",
                        message: format!(
                            "parent '{}' children must use compact indexes starting at zero",
                            parent_id.raw()
                        ),
                        entity_id: Some(link.edge_id),
                    });
                    break;
                }
            }

            if let Ok(limit) = effective_max_children(definition, state, *parent_id) {
                if links.len() > limit {
                    issues.push(TreeIssue {
                        code: "max_children_exceeded",
                        message: format!(
                            "parent '{}' has {} children but limit is {}",
                            parent_id.raw(),
                            links.len(),
                            limit
                        ),
                        entity_id: Some(*parent_id),
                    });
                }
            }
        }

        let mut depths = BTreeMap::new();
        let mut visited = BTreeSet::new();
        let mut stack = BTreeSet::new();
        if state.nodes.contains_key(&root_id) {
            validate_tree_cycles(
                root_id,
                0,
                state,
                &mut visited,
                &mut stack,
                &mut depths,
                &mut issues,
            );
        }

        for node_id in state.nodes.keys() {
            if !depths.contains_key(node_id) {
                issues.push(TreeIssue {
                    code: "unreachable_node",
                    message: format!(
                        "node '{}' is not reachable from root '{}'",
                        node_id.raw(),
                        root_id.raw()
                    ),
                    entity_id: Some(*node_id),
                });
            }
        }

        TreeValidation { issues, depths }
    }

    fn ensure_tree_operable(
        &self,
        definition: &crate::physical::PhysicalTreeDefinition,
        state: &TreeState,
    ) -> RedDBResult<()> {
        let validation = self.validate_tree_state(definition, state);
        let blocking_issue = validation.issues.into_iter().find(|issue| {
            !matches!(
                issue.code,
                "duplicate_child_index" | "non_compact_child_index"
            )
        });
        if let Some(issue) = blocking_issue {
            return Err(RedDBError::Query(format!(
                "tree '{}.{}' is not operable: {}",
                definition.collection, definition.name, issue.message
            )));
        }
        Ok(())
    }

    fn rewrite_parent_children(
        &self,
        collection: &str,
        tree_name: &str,
        parent_id: EntityId,
        child_ids: &[EntityId],
        current_state: &TreeState,
    ) -> RedDBResult<()> {
        if let Some(existing_links) = current_state.children_by_parent.get(&parent_id) {
            for link in existing_links {
                let _ = self.delete_entity_internal(collection, link.edge_id)?;
            }
        }
        for (index, child_id) in child_ids.iter().enumerate() {
            self.create_edge_unchecked(self.build_tree_structural_edge_input(
                collection, tree_name, parent_id, *child_id, index,
            ))?;
        }
        Ok(())
    }

    fn graph_edges_touching_nodes(
        &self,
        collection: &str,
        nodes: &BTreeSet<EntityId>,
    ) -> RedDBResult<BTreeSet<EntityId>> {
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection(collection) else {
            return Ok(BTreeSet::new());
        };
        let mut edge_ids = BTreeSet::new();
        for entity in manager.query_all(|entity| matches!(entity.kind, EntityKind::GraphEdge(_))) {
            let EntityKind::GraphEdge(edge_kind) = &entity.kind else {
                continue;
            };
            let Some(from_id) = parse_tree_entity_id_text(&edge_kind.from_node) else {
                continue;
            };
            let Some(to_id) = parse_tree_entity_id_text(&edge_kind.to_node) else {
                continue;
            };
            if nodes.contains(&from_id) || nodes.contains(&to_id) {
                edge_ids.insert(entity.id);
            }
        }
        Ok(edge_ids)
    }

    fn delete_entity_internal(&self, collection: &str, id: EntityId) -> RedDBResult<bool> {
        let store = self.inner.db.store();
        let deleted = store
            .delete(collection, id)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        if deleted {
            store.context_index().remove_entity(id);
            self.cdc_emit(
                crate::replication::cdc::ChangeOperation::Delete,
                collection,
                id.raw(),
                "entity",
            );
        }
        Ok(deleted)
    }

    fn plan_tree_rebalance(
        &self,
        definition: &crate::physical::PhysicalTreeDefinition,
        state: &TreeState,
    ) -> RedDBResult<Vec<TreeRebalancePlanRow>> {
        let validation = self.validate_tree_state(definition, state);
        let root_id = EntityId::new(definition.root_id);
        if !validation.depths.contains_key(&root_id) {
            return Err(RedDBError::Query(format!(
                "tree '{}.{}' is missing its root '{}'",
                definition.collection, definition.name, definition.root_id
            )));
        }

        let ordered = flatten_tree_preorder(state, root_id);
        let mut assignment_queue = VecDeque::new();
        assignment_queue.push_back(root_id);
        let mut assigned_children_count: BTreeMap<EntityId, usize> = BTreeMap::new();
        let mut new_depths = BTreeMap::new();
        new_depths.insert(root_id, 0usize);

        let mut plan = Vec::with_capacity(ordered.len());
        for node_id in ordered {
            loop {
                let Some(candidate_parent) = assignment_queue.front().copied() else {
                    return Err(RedDBError::Query(format!(
                        "tree '{}.{}' does not have enough aggregate capacity to rebalance",
                        definition.collection, definition.name
                    )));
                };
                let assigned = assigned_children_count
                    .get(&candidate_parent)
                    .copied()
                    .unwrap_or(0);
                let limit = effective_max_children(definition, state, candidate_parent)?;
                if assigned < limit {
                    break;
                }
                assignment_queue.pop_front();
            }

            let parent_id = assignment_queue.front().copied().ok_or_else(|| {
                RedDBError::Query(format!(
                    "tree '{}.{}' does not have enough aggregate capacity to rebalance",
                    definition.collection, definition.name
                ))
            })?;
            let entry = assigned_children_count.entry(parent_id).or_insert(0);
            let new_index = *entry;
            *entry += 1;
            let parent_depth = new_depths.get(&parent_id).copied().unwrap_or(0);
            new_depths.insert(node_id, parent_depth + 1);
            assignment_queue.push_back(node_id);

            let old_link = state.parent_by_child.get(&node_id).ok_or_else(|| {
                RedDBError::Query(format!(
                    "node '{}' is missing a structural parent in tree '{}.{}'",
                    node_id.raw(),
                    definition.collection,
                    definition.name
                ))
            })?;
            let old_depth = validation.depths.get(&node_id).copied().unwrap_or(0);
            let new_depth = new_depths.get(&node_id).copied().unwrap_or(0);
            let changed = old_link.parent_id != parent_id
                || old_link.child_index != new_index
                || old_depth != new_depth;

            plan.push(TreeRebalancePlanRow {
                node_id,
                old_parent_id: old_link.parent_id,
                new_parent_id: parent_id,
                old_index: old_link.child_index,
                new_index,
                old_depth,
                new_depth,
                changed,
            });
        }

        Ok(plan)
    }
}

fn current_tree_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn build_tree_node_metadata(
    tree_name: &str,
    metadata_entries: &[(String, Value)],
) -> RedDBResult<Vec<(String, MetadataValue)>> {
    let mut metadata = Vec::with_capacity(metadata_entries.len() + 1);
    metadata.push((
        TREE_METADATA_NAME.to_string(),
        MetadataValue::String(tree_name.to_string()),
    ));
    for (key, value) in metadata_entries {
        metadata.push((key.clone(), storage_value_to_tree_metadata(value)?));
    }
    Ok(metadata)
}

fn storage_value_to_tree_metadata(value: &Value) -> RedDBResult<MetadataValue> {
    Ok(match value {
        Value::Null => MetadataValue::Null,
        Value::Boolean(value) => MetadataValue::Bool(*value),
        Value::Integer(value) => MetadataValue::Int(*value),
        Value::UnsignedInteger(value) => {
            if *value <= i64::MAX as u64 {
                MetadataValue::Int(*value as i64)
            } else {
                MetadataValue::Timestamp(*value)
            }
        }
        Value::Float(value) => MetadataValue::Float(*value),
        Value::Text(value) => MetadataValue::String(value.to_string()),
        Value::Blob(value) => MetadataValue::Bytes(value.clone()),
        Value::Timestamp(value) => {
            if *value < 0 {
                return Err(RedDBError::Query(
                    "negative timestamps are not supported in tree metadata".to_string(),
                ));
            }
            MetadataValue::Timestamp(*value as u64)
        }
        Value::Json(bytes) => {
            let json = crate::json::from_slice::<crate::json::Value>(bytes).map_err(|err| {
                RedDBError::Query(format!("failed to decode JSON metadata value: {err}"))
            })?;
            json_to_metadata_value(&json)?
        }
        Value::Vector(values) => MetadataValue::Array(
            values
                .iter()
                .map(|value| MetadataValue::Float(*value as f64))
                .collect(),
        ),
        Value::Array(values) => MetadataValue::Array(
            values
                .iter()
                .map(storage_value_to_tree_metadata)
                .collect::<RedDBResult<Vec<_>>>()?,
        ),
        other => MetadataValue::String(format!("{other:?}")),
    })
}

fn resolve_tree_insert_position(position: TreePosition, len: usize) -> RedDBResult<usize> {
    match position {
        TreePosition::First => Ok(0),
        TreePosition::Last => Ok(len),
        TreePosition::Index(index) if index <= len => Ok(index),
        TreePosition::Index(index) => Err(RedDBError::Query(format!(
            "tree child position {} is out of bounds for {} children",
            index, len
        ))),
    }
}

fn child_id_list(
    children_by_parent: &BTreeMap<EntityId, Vec<TreeChildLink>>,
    parent_id: EntityId,
) -> Vec<EntityId> {
    children_by_parent
        .get(&parent_id)
        .map(|links| links.iter().map(|link| link.child_id).collect())
        .unwrap_or_default()
}

fn subtree_ids(state: &TreeState, root_id: EntityId) -> BTreeSet<EntityId> {
    let mut ids = BTreeSet::new();
    let mut queue = VecDeque::new();
    queue.push_back(root_id);
    while let Some(node_id) = queue.pop_front() {
        if !ids.insert(node_id) {
            continue;
        }
        if let Some(children) = state.children_by_parent.get(&node_id) {
            for child in children {
                queue.push_back(child.child_id);
            }
        }
    }
    ids
}

fn flatten_tree_preorder(state: &TreeState, root_id: EntityId) -> Vec<EntityId> {
    let mut ordered = Vec::new();
    flatten_tree_preorder_inner(state, root_id, &mut ordered);
    ordered
}

fn flatten_tree_preorder_inner(state: &TreeState, current: EntityId, ordered: &mut Vec<EntityId>) {
    if let Some(children) = state.children_by_parent.get(&current) {
        for child in children {
            ordered.push(child.child_id);
            flatten_tree_preorder_inner(state, child.child_id, ordered);
        }
    }
}

fn validate_tree_cycles(
    node_id: EntityId,
    depth: usize,
    state: &TreeState,
    visited: &mut BTreeSet<EntityId>,
    stack: &mut BTreeSet<EntityId>,
    depths: &mut BTreeMap<EntityId, usize>,
    issues: &mut Vec<TreeIssue>,
) {
    if stack.contains(&node_id) {
        issues.push(TreeIssue {
            code: "cycle_detected",
            message: format!("cycle detected at node '{}'", node_id.raw()),
            entity_id: Some(node_id),
        });
        return;
    }
    if !visited.insert(node_id) {
        return;
    }
    stack.insert(node_id);
    depths.insert(node_id, depth);
    if let Some(children) = state.children_by_parent.get(&node_id) {
        for child in children {
            validate_tree_cycles(
                child.child_id,
                depth + 1,
                state,
                visited,
                stack,
                depths,
                issues,
            );
        }
    }
    stack.remove(&node_id);
}

fn effective_max_children(
    definition: &crate::physical::PhysicalTreeDefinition,
    state: &TreeState,
    node_id: EntityId,
) -> RedDBResult<usize> {
    let Some(node) = state.nodes.get(&node_id) else {
        return Err(RedDBError::NotFound(format!(
            "node '{}' was not found in tree '{}.{}'",
            node_id.raw(),
            definition.collection,
            definition.name
        )));
    };
    if let Some(value) = node.max_children_override {
        if value == 0 {
            return Err(RedDBError::Query(format!(
                "node '{}' has non-positive max children override",
                node_id.raw()
            )));
        }
        return Ok(value);
    }
    Ok(definition.default_max_children)
}

fn metadata_tree_name(metadata: &Metadata) -> Option<&str> {
    match metadata.get(TREE_METADATA_NAME) {
        Some(MetadataValue::String(value)) => Some(value.as_str()),
        _ => None,
    }
}

fn metadata_usize(metadata: &Metadata, key: &str) -> Option<usize> {
    match metadata.get(key) {
        Some(MetadataValue::Int(value)) if *value >= 0 => Some(*value as usize),
        Some(MetadataValue::Timestamp(value)) => (*value).try_into().ok(),
        _ => None,
    }
}

fn metadata_invalid_max_children(metadata: &Metadata) -> bool {
    match metadata.get(TREE_METADATA_MAX_CHILDREN) {
        None => false,
        Some(MetadataValue::Int(value)) => *value <= 0,
        Some(MetadataValue::Timestamp(value)) => *value == 0,
        _ => true,
    }
}

fn parse_tree_entity_id_text(value: &str) -> Option<EntityId> {
    value
        .strip_prefix('e')
        .unwrap_or(value)
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .map(EntityId::new)
}
