//! Single-owner local SQL. Native formats and publication barriers remain canonical.
use super::{
    local_read::{query_resident_bound, query_resident_scoped, LocalReadError, ReadScope},
    scalar::ScalarResult,
};
use crate::{
    catalog::{
        codec::{deserialize_collection_contracts, serialize_collection_contracts},
        CollectionContract,
    },
    storage::{
        manager::ManagerConfig, resident_file::EmbeddedResidentStore,
        resident_store::ResidentStore, store_wal::StoreWalAction,
    },
};
use reddb_file::VfsFile;
use reddb_rql::ast::{QueryExpr, TxnControl};
use reddb_types::Value;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

mod mutation;

type Result<T> = std::result::Result<T, LocalReadError>;

/// Same positional-parameter count bound as the native wire protocol.
pub const MAX_QUERY_PARAMETERS: usize = 65_536;

struct Transaction {
    resident: ResidentStore,
    contracts: HashMap<String, CollectionContract>,
    actions: Vec<StoreWalAction>,
    catalog_changed: bool,
    aborted: bool,
}

/// One exclusively locked database handle is the transaction/session boundary.
/// Closing drops uncommitted state. Failed I/O fences the controller until reopen.
pub struct LocalDatabase<F: VfsFile> {
    store: EmbeddedResidentStore<F>,
    contracts: HashMap<String, CollectionContract>,
    transaction: Option<Transaction>,
}

impl<F: VfsFile> LocalDatabase<F> {
    pub fn create(file: F, name: impl Into<PathBuf>, now_ms: u64) -> Result<Self> {
        Ok(Self {
            store: EmbeddedResidentStore::create(
                file,
                name,
                u128::from(now_ms),
                ManagerConfig::default(),
                false,
                0,
            )?,
            contracts: HashMap::new(),
            transaction: None,
        })
    }

    pub fn open(file: F, name: impl Into<PathBuf>) -> Result<Self> {
        let store = EmbeddedResidentStore::open(file, name, ManagerConfig::default(), false, 0)?;
        let bytes = store.committed_resident()?.aux_metadata();
        let mut contracts = HashMap::new();
        if !bytes.is_empty() {
            for contract in deserialize_collection_contracts(&bytes)
                .map_err(|error| LocalReadError::Catalog(error.to_string()))?
            {
                let name = contract.name.clone();
                if contracts.insert(name.clone(), contract).is_some() {
                    return Err(LocalReadError::Catalog(format!(
                        "duplicate collection contract: {name}"
                    )));
                }
            }
        }
        Ok(Self {
            store,
            contracts,
            transaction: None,
        })
    }

    pub fn query(&mut self, sql: &str, now_ms: u64) -> Result<ScalarResult> {
        self.query_with_params(sql, &[], now_ms)
    }

    /// Host-side read operations must honor the same interrupted-publication fence.
    pub fn check_healthy(&self) -> Result<()> {
        self.store.committed_resident()?;
        Ok(())
    }

    /// Read actual persisted contracts without inferring schemas from row payloads.
    pub fn catalog(&self) -> Result<ScalarResult> {
        self.check_healthy()?;
        if self
            .transaction
            .as_ref()
            .is_some_and(|transaction| transaction.aborted)
        {
            return Err(query_error("transaction is aborted; ROLLBACK is required"));
        }
        let (_, contracts) = self.visible()?;
        let mut tables: Vec<_> = contracts.values().collect();
        tables.sort_by(|left, right| left.name.cmp(&right.name));
        let mut rows = Vec::new();
        for table in tables {
            for column in crate::catalog::schema::resolved_contract_columns(table)
                .map_err(|error| LocalReadError::Catalog(error.to_string()))?
            {
                rows.push(vec![
                    Value::text(table.name.as_str()),
                    Value::text(column.name),
                    Value::text(column.data_type_name),
                    Value::Boolean(!column.not_null),
                ]);
            }
        }
        Ok(ScalarResult {
            columns: ["table_name", "column_name", "data_type", "nullable"]
                .map(String::from)
                .to_vec(),
            rows,
        })
    }

    /// Only SELECT is admitted; a caller cannot widen the host's source-row scope.
    pub fn read_query(
        &self,
        sql: &str,
        parameters: &[Value],
        scopes: &[ReadScope],
    ) -> Result<ScalarResult> {
        self.check_healthy()?;
        if parameters.len() > MAX_QUERY_PARAMETERS || scopes.len() > 64 {
            return Err(query_error("inspection arguments exceed their limit"));
        }
        if self
            .transaction
            .as_ref()
            .is_some_and(|transaction| transaction.aborted)
        {
            return Err(query_error("transaction is aborted; ROLLBACK is required"));
        }
        let statement = reddb_rql::parse_frontend(sql)
            .map_err(LocalReadError::Parse)?
            .into_query_expr();
        let statement =
            super::parameters::bind(&statement, parameters).map_err(LocalReadError::Parameters)?;
        let QueryExpr::Table(query) = statement else {
            return Err(LocalReadError::Unsupported(
                "inspection accepts table SELECT only",
            ));
        };
        let mut tables = HashSet::new();
        if scopes.iter().any(|scope| !tables.insert(&scope.table)) {
            return Err(query_error("duplicate inspection table scope"));
        }
        let scope = scopes
            .iter()
            .find(|scope| scope.table == query.table)
            .ok_or(LocalReadError::Unsupported(
                "table is outside the inspection scope",
            ))?;
        let (resident, contracts) = self.visible()?;
        query_resident_scoped(resident, contracts, query, scope)
    }

    pub fn query_with_params(
        &mut self,
        sql: &str,
        parameters: &[Value],
        now_ms: u64,
    ) -> Result<ScalarResult> {
        let result = self.query_inner(sql, parameters, now_ms);
        if result.is_err() {
            if let Some(transaction) = &mut self.transaction {
                transaction.aborted = true;
            }
        }
        result
    }

    fn query_inner(
        &mut self,
        sql: &str,
        parameters: &[Value],
        now_ms: u64,
    ) -> Result<ScalarResult> {
        // Also fence staged reads after an ambiguous durable commit.
        self.check_healthy()?;
        if parameters.len() > MAX_QUERY_PARAMETERS {
            return Err(query_error("local SQL accepts at most 65536 parameters"));
        }
        let statement = reddb_rql::parse_frontend(sql)
            .map_err(LocalReadError::Parse)?
            .into_query_expr();
        let statement =
            super::parameters::bind(&statement, parameters).map_err(LocalReadError::Parameters)?;
        if matches!(
            statement,
            QueryExpr::TransactionControl(TxnControl::Rollback)
        ) {
            if self.transaction.take().is_none() {
                return Err(query_error("no active transaction"));
            }
            return Ok(affected(0));
        }
        if self
            .transaction
            .as_ref()
            .is_some_and(|transaction| transaction.aborted)
        {
            return Err(query_error("transaction is aborted; ROLLBACK is required"));
        }
        match statement {
            QueryExpr::TransactionControl(control) => self.control(control),
            QueryExpr::Table(query) => {
                let (resident, contracts) = self.visible()?;
                query_resident_bound(resident, contracts, query)
            }
            QueryExpr::CreateTable(query) => self.create_table(&query, now_ms),
            QueryExpr::Insert(_) | QueryExpr::Update(_) | QueryExpr::Delete(_) => {
                let (resident, contracts) = self.visible()?;
                let actions = mutation::plan(&statement, resident, contracts, now_ms)?;
                let count = actions.len();
                if let Some(transaction) = &mut self.transaction {
                    if transaction.actions.len().checked_add(count).is_none_or(|count| count > super::local_read::LOCAL_READ_ROWS_MAX) {
                        return Err(LocalReadError::ResourceLimit);
                    }
                    // An apply error aborts the transaction; provisional state is
                    // inaccessible until rollback, including through COMMIT.
                    apply(&transaction.resident, &actions)?;
                    transaction.actions.extend(actions);
                } else {
                    self.store.publish_actions(&actions)?;
                }
                Ok(affected(count))
            }
            _ => Err(LocalReadError::Unsupported("local SQL supports table SELECT, CREATE TABLE, INSERT, UPDATE, DELETE and BEGIN/COMMIT/ROLLBACK")),
        }
    }

    fn visible(&self) -> Result<(&ResidentStore, &HashMap<String, CollectionContract>)> {
        match &self.transaction {
            Some(transaction) => Ok((&transaction.resident, &transaction.contracts)),
            None => Ok((self.store.committed_resident()?, &self.contracts)),
        }
    }

    fn control(&mut self, control: TxnControl) -> Result<ScalarResult> {
        match control {
            TxnControl::Begin(None) => {
                if self.transaction.is_some() {
                    return Err(query_error("a transaction is already active"));
                }
                self.transaction = Some(Transaction {
                    resident: copy_resident(self.store.committed_resident()?)?,
                    contracts: self.contracts.clone(),
                    actions: Vec::new(),
                    catalog_changed: false,
                    aborted: false,
                });
            }
            TxnControl::Commit => {
                let transaction = self.transaction.as_ref().ok_or_else(|| query_error("no active transaction"))?;
                if transaction.catalog_changed {
                    self.store.publish_snapshot(&snapshot(&transaction.resident))?;
                } else {
                    self.store.publish_actions(&transaction.actions)?;
                }
                self.contracts = self.transaction.take().expect("active transaction committed").contracts;
            }
            _ => return Err(LocalReadError::Unsupported("local transactions support BEGIN, COMMIT and ROLLBACK without isolation options or savepoints")),
        }
        Ok(affected(0))
    }

    fn create_table(
        &mut self,
        query: &reddb_rql::ast::CreateTableQuery,
        now_ms: u64,
    ) -> Result<ScalarResult> {
        mutation::admit_create(query)?;
        let (exists, catalog_exists) = {
            let (resident, contracts) = self.visible()?;
            (
                resident.get_collection(&query.name).is_some(),
                contracts.contains_key(&query.name),
            )
        };
        if exists || catalog_exists {
            if query.if_not_exists && catalog_exists {
                return Ok(affected(0));
            }
            return Err(query_error(format!(
                "table '{}' already exists",
                query.name
            )));
        }
        let contract =
            crate::catalog::ddl::collection_contract_from_create_table(query, u128::from(now_ms))
                .map_err(query_error)?;
        crate::catalog::schema::validate_contract_expressions(&contract)
            .map_err(|error| query_error(error.message))?;
        let action = StoreWalAction::CreateCollection {
            name: query.name.clone(),
        };
        if let Some(transaction) = &mut self.transaction {
            apply(&transaction.resident, &[action])?;
            transaction.contracts.insert(query.name.clone(), contract);
            set_contracts(&transaction.resident, &transaction.contracts);
            transaction.catalog_changed = true;
        } else {
            let (resident, contracts) = self.visible()?;
            let staged = copy_resident(resident)?;
            let mut contracts = contracts.clone();
            apply(&staged, &[action])?;
            contracts.insert(query.name.clone(), contract);
            set_contracts(&staged, &contracts);
            self.store.publish_snapshot(&snapshot(&staged))?;
            self.contracts = contracts;
        }
        Ok(affected(0))
    }

    pub fn into_inner(self) -> F {
        self.store.into_inner()
    }
}

fn affected(count: usize) -> ScalarResult {
    ScalarResult {
        columns: vec!["affectedRows".into()],
        rows: vec![vec![Value::UnsignedInteger(
            u64::try_from(count).expect("row count fits u64"),
        )]],
    }
}
fn query_error(message: impl Into<String>) -> LocalReadError {
    LocalReadError::Query(message.into())
}
fn snapshot(resident: &ResidentStore) -> Vec<u8> {
    resident.snapshot_bytes(&HashSet::new(), &HashSet::new(), || {})
}
fn copy_resident(resident: &ResidentStore) -> Result<ResidentStore> {
    ResidentStore::from_snapshot(&snapshot(resident), &ManagerConfig::default(), false, 0)
        .map(|(resident, _)| resident)
        .map_err(|error| query_error(error.to_string()))
}
fn apply(resident: &ResidentStore, actions: &[StoreWalAction]) -> Result<()> {
    for action in actions {
        resident.apply_replayed_action(
            action,
            reddb_file::STORE_VERSION_CURRENT,
            &ManagerConfig::default(),
        )?;
    }
    Ok(())
}
fn set_contracts(resident: &ResidentStore, contracts: &HashMap<String, CollectionContract>) {
    let mut contracts: Vec<_> = contracts.values().cloned().collect();
    contracts.sort_by(|left, right| left.name.cmp(&right.name));
    resident.set_aux_metadata(serialize_collection_contracts(&contracts));
}
