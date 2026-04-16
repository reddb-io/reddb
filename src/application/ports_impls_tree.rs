use super::*;
use crate::application::entity::metadata_to_json;
use crate::storage::query::ast::{
    CreateTreeQuery, DropTreeQuery, TreeCommand, TreeNodeSpec, TreePosition,
};

fn api_tree_query(label: &str, collection: &str, tree: &str) -> String {
    format!("api.{label}({collection}.{tree})")
}

fn tree_position(position: crate::application::TreePositionInput) -> TreePosition {
    match position {
        crate::application::TreePositionInput::First => TreePosition::First,
        crate::application::TreePositionInput::Last => TreePosition::Last,
        crate::application::TreePositionInput::Index(index) => TreePosition::Index(index),
    }
}

fn tree_node_spec(input: crate::application::TreeNodeInput) -> RedDBResult<TreeNodeSpec> {
    let metadata = input
        .metadata
        .into_iter()
        .map(|(key, value)| {
            let json = metadata_to_json(&crate::storage::unified::Metadata::with_fields(
                [(key.clone(), value)].into_iter().collect(),
            ));
            let value = json
                .get(&key)
                .ok_or_else(|| {
                    crate::RedDBError::Query(format!(
                        "failed to convert tree metadata field '{}'",
                        key
                    ))
                })
                .and_then(crate::application::entity::json_to_storage_value)?;
            Ok((key, value))
        })
        .collect::<RedDBResult<Vec<_>>>()?;

    Ok(TreeNodeSpec {
        label: input.label,
        node_type: input.node_type,
        properties: input.properties,
        metadata,
        max_children: input.max_children,
    })
}

impl RuntimeTreePort for RedDBRuntime {
    fn create_tree(&self, input: CreateTreeInput) -> RedDBResult<RuntimeQueryResult> {
        let raw_query = api_tree_query("create_tree", &input.collection, &input.name);
        let query = CreateTreeQuery {
            collection: input.collection,
            name: input.name,
            root: tree_node_spec(input.root)?,
            default_max_children: input.default_max_children,
            if_not_exists: input.if_not_exists,
        };
        RedDBRuntime::execute_create_tree(self, &raw_query, &query)
    }

    fn drop_tree(&self, input: DropTreeInput) -> RedDBResult<RuntimeQueryResult> {
        let raw_query = api_tree_query("drop_tree", &input.collection, &input.name);
        let query = DropTreeQuery {
            collection: input.collection,
            name: input.name,
            if_exists: input.if_exists,
        };
        RedDBRuntime::execute_drop_tree(self, &raw_query, &query)
    }

    fn insert_tree_node(&self, input: InsertTreeNodeInput) -> RedDBResult<RuntimeQueryResult> {
        let raw_query = api_tree_query("tree_insert", &input.collection, &input.tree_name);
        let command = TreeCommand::Insert {
            collection: input.collection,
            tree_name: input.tree_name,
            parent_id: input.parent_id,
            node: tree_node_spec(input.node)?,
            position: tree_position(input.position),
        };
        RedDBRuntime::execute_tree_command(self, &raw_query, &command)
    }

    fn move_tree_node(&self, input: MoveTreeNodeInput) -> RedDBResult<RuntimeQueryResult> {
        let raw_query = api_tree_query("tree_move", &input.collection, &input.tree_name);
        let command = TreeCommand::Move {
            collection: input.collection,
            tree_name: input.tree_name,
            node_id: input.node_id,
            parent_id: input.parent_id,
            position: tree_position(input.position),
        };
        RedDBRuntime::execute_tree_command(self, &raw_query, &command)
    }

    fn delete_tree_node(&self, input: DeleteTreeNodeInput) -> RedDBResult<RuntimeQueryResult> {
        let raw_query = api_tree_query("tree_delete", &input.collection, &input.tree_name);
        let command = TreeCommand::Delete {
            collection: input.collection,
            tree_name: input.tree_name,
            node_id: input.node_id,
        };
        RedDBRuntime::execute_tree_command(self, &raw_query, &command)
    }

    fn validate_tree(&self, input: ValidateTreeInput) -> RedDBResult<RuntimeQueryResult> {
        let raw_query = api_tree_query("tree_validate", &input.collection, &input.tree_name);
        let command = TreeCommand::Validate {
            collection: input.collection,
            tree_name: input.tree_name,
        };
        RedDBRuntime::execute_tree_command(self, &raw_query, &command)
    }

    fn rebalance_tree(&self, input: RebalanceTreeInput) -> RedDBResult<RuntimeQueryResult> {
        let raw_query = api_tree_query("tree_rebalance", &input.collection, &input.tree_name);
        let command = TreeCommand::Rebalance {
            collection: input.collection,
            tree_name: input.tree_name,
            dry_run: input.dry_run,
        };
        RedDBRuntime::execute_tree_command(self, &raw_query, &command)
    }
}
