//! Single-owner local SQL. Native formats and publication barriers remain canonical.
use super::{
    local_read::{
        query_resident_bound, query_resident_scoped, query_store_stream, source::StoreView,
        LocalReadError, ReadScope, StreamQueryResult,
    },
    scalar::ScalarResult,
};
use crate::{
    catalog::{
        codec::{deserialize_collection_contracts, serialize_collection_contracts},
        CollectionContract,
    },
    storage::store_wal::StoreWalAction,
};
use reddb_file::VfsFile;
use reddb_rql::ast::{QueryExpr, TxnControl};
use reddb_types::Value;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

mod mutation;
#[cfg(not(target_arch = "wasm32"))]
pub mod service;
mod strategy;
pub mod vault;

use strategy::{PagedStrategy, ResidentStrategy, StorageStrategy};

type Result<T> = std::result::Result<T, LocalReadError>;

/// Same positional-parameter count bound as the native wire protocol.
pub const MAX_QUERY_PARAMETERS: usize = 65_536;

struct Transaction<T> {
    store: T,
    contracts: HashMap<String, CollectionContract>,
    actions: Vec<StoreWalAction>,
    catalog_changed: bool,
    aborted: bool,
}

/// One exclusively locked database handle is the transaction/session boundary.
/// Closing drops uncommitted state. Failed I/O fences the controller until reopen.
pub struct LocalDatabase<F: VfsFile> {
    database: Database<ResidentStrategy<F>>,
}

/// Bounded-page local SQL over the canonical `.rdb` pager and B-tree.
/// Transactions and non-append mutations remain explicitly unsupported.
#[doc(hidden)]
pub struct PagedLocalDatabase<F: VfsFile + 'static> {
    database: Database<PagedStrategy<F>>,
}

struct Database<S: StorageStrategy> {
    storage: S,
    contracts: HashMap<String, CollectionContract>,
    transaction: Option<Transaction<S::Transaction>>,
}

impl<F: VfsFile> LocalDatabase<F> {
    pub fn create(file: F, name: impl Into<PathBuf>, now_ms: u64) -> Result<Self> {
        Ok(Self {
            database: Database::create(ResidentStrategy::create(file, name, now_ms)?),
        })
    }

    pub fn open(file: F, name: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self {
            database: Database::open(ResidentStrategy::open(file, name)?)?,
        })
    }

    pub fn query(&mut self, sql: &str, now_ms: u64) -> Result<ScalarResult> {
        self.database.query(sql, now_ms)
    }

    /// Host-side read operations honor the interrupted-publication fence.
    pub fn check_healthy(&self) -> Result<()> {
        self.database.check_healthy()
    }

    /// Read persisted contracts without inferring schemas from row payloads.
    pub fn catalog(&self) -> Result<ScalarResult> {
        self.database.catalog()
    }

    pub fn query_stream(
        &self,
        sql: &str,
        parameters: &[Value],
        visit: &mut dyn FnMut(&[Value]) -> Result<bool>,
    ) -> Result<StreamQueryResult> {
        self.database.query_stream(sql, parameters, visit)
    }

    /// Only SELECT is admitted; callers cannot widen the host's row scope.
    pub fn read_query(
        &self,
        sql: &str,
        parameters: &[Value],
        scopes: &[ReadScope],
    ) -> Result<ScalarResult> {
        self.database.read_query(sql, parameters, scopes)
    }

    pub fn query_with_params(
        &mut self,
        sql: &str,
        parameters: &[Value],
        now_ms: u64,
    ) -> Result<ScalarResult> {
        self.database.query_with_params(sql, parameters, now_ms)
    }

    pub fn into_inner(self) -> F {
        self.database.storage.into_inner()
    }
}

impl<F: VfsFile + 'static> PagedLocalDatabase<F> {
    pub fn create(file: F) -> Result<Self> {
        Ok(Self {
            database: Database::create(PagedStrategy::create(file)?),
        })
    }

    pub fn open(file: F) -> Result<Self> {
        Ok(Self {
            database: Database::open(PagedStrategy::open(file)?)?,
        })
    }

    pub fn query(&mut self, sql: &str, now_ms: u64) -> Result<ScalarResult> {
        self.database.query(sql, now_ms)
    }

    pub fn query_with_params(
        &mut self,
        sql: &str,
        parameters: &[Value],
        now_ms: u64,
    ) -> Result<ScalarResult> {
        self.database.query_with_params(sql, parameters, now_ms)
    }

    pub fn catalog(&self) -> Result<ScalarResult> {
        self.database.catalog()
    }

    pub fn query_stream(
        &self,
        sql: &str,
        parameters: &[Value],
        visit: &mut dyn FnMut(&[Value]) -> Result<bool>,
    ) -> Result<StreamQueryResult> {
        self.database.query_stream(sql, parameters, visit)
    }

    pub fn cache_pages(&self) -> usize {
        self.database.storage.cache_pages()
    }

    pub fn into_inner(self) -> F {
        self.database.storage.into_inner()
    }
}

impl<S: StorageStrategy> Database<S> {
    fn create(storage: S) -> Self {
        Self {
            storage,
            contracts: HashMap::new(),
            transaction: None,
        }
    }

    fn open(storage: S) -> Result<Self> {
        let bytes = storage.committed_view()?.aux_metadata()?;
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
            storage,
            contracts,
            transaction: None,
        })
    }

    fn query(&mut self, sql: &str, now_ms: u64) -> Result<ScalarResult> {
        self.query_with_params(sql, &[], now_ms)
    }

    /// Host-side read operations must honor the same interrupted-publication fence.
    fn check_healthy(&self) -> Result<()> {
        self.storage.committed_view()?;
        Ok(())
    }

    /// Read actual persisted contracts without inferring schemas from row payloads.
    fn catalog(&self) -> Result<ScalarResult> {
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
            if table.declared_model != crate::catalog::CollectionModel::Table {
                continue;
            }
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
    fn read_query(
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

    fn query_stream(
        &self,
        sql: &str,
        parameters: &[Value],
        visit: &mut dyn FnMut(&[Value]) -> Result<bool>,
    ) -> Result<StreamQueryResult> {
        self.check_healthy()?;
        if parameters.len() > MAX_QUERY_PARAMETERS {
            return Err(query_error("local SQL accepts at most 65536 parameters"));
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
                "streaming execution requires SELECT",
            ));
        };
        let (store, contracts) = self.visible()?;
        query_store_stream(store, contracts, query, visit)
    }

    fn query_with_params(
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
        self.storage.admit(&statement)?;
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
                    self.storage.apply_transaction(&transaction.store, &actions)?;
                    transaction.actions.extend(actions);
                } else {
                    self.storage.publish_actions(&actions)?;
                }
                Ok(affected(count))
            }
            _ => Err(LocalReadError::Unsupported("local SQL supports table SELECT, CREATE TABLE, INSERT, UPDATE, DELETE and BEGIN/COMMIT/ROLLBACK")),
        }
    }

    fn visible(&self) -> Result<(&dyn StoreView, &HashMap<String, CollectionContract>)> {
        match &self.transaction {
            Some(transaction) => Ok((&transaction.store, &transaction.contracts)),
            None => Ok((self.storage.committed_view()?, &self.contracts)),
        }
    }

    fn control(&mut self, control: TxnControl) -> Result<ScalarResult> {
        match control {
            TxnControl::Begin(None) => {
                if self.transaction.is_some() {
                    return Err(query_error("a transaction is already active"));
                }
                self.transaction = Some(Transaction {
                    store: self.storage.begin_transaction()?,
                    contracts: self.contracts.clone(),
                    actions: Vec::new(),
                    catalog_changed: false,
                    aborted: false,
                });
            }
            TxnControl::Commit => {
                let transaction = self.transaction.as_ref().ok_or_else(|| query_error("no active transaction"))?;
                let catalog = transaction
                    .catalog_changed
                    .then(|| catalog_bytes(&transaction.contracts));
                self.storage.commit_transaction(
                    &transaction.store,
                    &transaction.actions,
                    catalog,
                )?;
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
                resident.contains_collection(&query.name)?,
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
            self.storage
                .apply_transaction(&transaction.store, &[action])?;
            transaction.contracts.insert(query.name.clone(), contract);
            transaction.catalog_changed = true;
        } else {
            let mut contracts = self.contracts.clone();
            contracts.insert(query.name.clone(), contract);
            self.storage
                .publish_catalog_actions(&[action], catalog_bytes(&contracts))?;
            self.contracts = contracts;
        }
        Ok(affected(0))
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
fn catalog_bytes(contracts: &HashMap<String, CollectionContract>) -> Vec<u8> {
    let mut contracts: Vec<_> = contracts.values().cloned().collect();
    contracts.sort_by(|left, right| left.name.cmp(&right.name));
    serialize_collection_contracts(&contracts)
}
