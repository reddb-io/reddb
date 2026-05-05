use crate::application::ports::RuntimeTreePort;
use crate::runtime::RuntimeQueryResult;
use crate::storage::schema::Value;
use crate::storage::unified::MetadataValue;
use crate::RedDBResult;

#[derive(Debug, Clone)]
pub struct TreeNodeInput {
    pub label: String,
    pub node_type: Option<String>,
    pub properties: Vec<(String, Value)>,
    pub metadata: Vec<(String, MetadataValue)>,
    pub max_children: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct CreateTreeInput {
    pub collection: String,
    pub name: String,
    pub root: TreeNodeInput,
    pub default_max_children: usize,
    pub if_not_exists: bool,
}

#[derive(Debug, Clone)]
pub struct DropTreeInput {
    pub collection: String,
    pub name: String,
    pub if_exists: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreePositionInput {
    First,
    Last,
    Index(usize),
}

#[derive(Debug, Clone)]
pub struct InsertTreeNodeInput {
    pub collection: String,
    pub tree_name: String,
    pub parent_id: u64,
    pub node: TreeNodeInput,
    pub position: TreePositionInput,
}

#[derive(Debug, Clone)]
pub struct MoveTreeNodeInput {
    pub collection: String,
    pub tree_name: String,
    pub node_id: u64,
    pub parent_id: u64,
    pub position: TreePositionInput,
}

#[derive(Debug, Clone)]
pub struct DeleteTreeNodeInput {
    pub collection: String,
    pub tree_name: String,
    pub node_id: u64,
}

#[derive(Debug, Clone)]
pub struct ValidateTreeInput {
    pub collection: String,
    pub tree_name: String,
}

#[derive(Debug, Clone)]
pub struct RebalanceTreeInput {
    pub collection: String,
    pub tree_name: String,
    pub dry_run: bool,
}

pub struct TreeUseCases<'a, P: ?Sized> {
    runtime: &'a P,
}

impl<'a, P: RuntimeTreePort + ?Sized> TreeUseCases<'a, P> {
    pub fn new(runtime: &'a P) -> Self {
        Self { runtime }
    }

    pub fn create_tree(&self, input: CreateTreeInput) -> RedDBResult<RuntimeQueryResult> {
        self.runtime.create_tree(input)
    }

    pub fn drop_tree(&self, input: DropTreeInput) -> RedDBResult<RuntimeQueryResult> {
        self.runtime.drop_tree(input)
    }

    pub fn insert_node(&self, input: InsertTreeNodeInput) -> RedDBResult<RuntimeQueryResult> {
        self.runtime.insert_tree_node(input)
    }

    pub fn move_node(&self, input: MoveTreeNodeInput) -> RedDBResult<RuntimeQueryResult> {
        self.runtime.move_tree_node(input)
    }

    pub fn delete_node(&self, input: DeleteTreeNodeInput) -> RedDBResult<RuntimeQueryResult> {
        self.runtime.delete_tree_node(input)
    }

    pub fn validate(&self, input: ValidateTreeInput) -> RedDBResult<RuntimeQueryResult> {
        self.runtime.validate_tree(input)
    }

    pub fn rebalance(&self, input: RebalanceTreeInput) -> RedDBResult<RuntimeQueryResult> {
        self.runtime.rebalance_tree(input)
    }
}
