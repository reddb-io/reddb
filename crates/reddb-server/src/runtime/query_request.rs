use super::{RedDBRuntime, RuntimeQueryResult};
use crate::storage::query::ast::QueryExpr;
use crate::storage::query::modes::parse_multi;
use crate::storage::query::user_params;
use crate::storage::schema::Value;
use crate::{RedDBError, RedDBResult};
use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const DEFAULT_PREPARED_CAPACITY: usize = 1_024;

/// Transport-neutral parameter value vocabulary from ADR 0015.
#[derive(Debug, Clone, PartialEq)]
pub enum ParamValue {
    Null,
    Bool(bool),
    Int64(i64),
    Float64(f64),
    Text(String),
    Bytes(Vec<u8>),
    Vector(Vec<f32>),
    Json(Vec<u8>),
    Timestamp(i64),
    Uuid([u8; 16]),
}

impl From<ParamValue> for Value {
    fn from(value: ParamValue) -> Self {
        match value {
            ParamValue::Null => Self::Null,
            ParamValue::Bool(value) => Self::Boolean(value),
            ParamValue::Int64(value) => Self::Integer(value),
            ParamValue::Float64(value) => Self::Float(value),
            ParamValue::Text(value) => Self::text(value),
            ParamValue::Bytes(value) => Self::Blob(value),
            ParamValue::Vector(value) => Self::Vector(value),
            ParamValue::Json(value) => Self::Json(value),
            ParamValue::Timestamp(value) => Self::Timestamp(value),
            ParamValue::Uuid(value) => Self::Uuid(value),
        }
    }
}

#[derive(Debug, Clone)]
enum QuerySource {
    Sql(String),
    Prepared(PreparedId),
}

#[derive(Debug, Clone)]
pub struct QueryRequest {
    source: QuerySource,
    params: Vec<ParamValue>,
    commit_policy: Option<crate::replication::CommitPolicy>,
}

impl QueryRequest {
    pub fn sql(sql: impl Into<String>, params: Vec<ParamValue>) -> Self {
        Self {
            source: QuerySource::Sql(sql.into()),
            params,
            commit_policy: None,
        }
    }

    pub fn prepared(prepared_id: PreparedId, params: Vec<ParamValue>) -> Self {
        Self {
            source: QuerySource::Prepared(prepared_id),
            params,
            commit_policy: None,
        }
    }

    pub fn with_commit_policy(mut self, commit_policy: crate::replication::CommitPolicy) -> Self {
        self.commit_policy = Some(commit_policy);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PreparedId(u64);

impl PreparedId {
    pub const fn from_raw(value: u64) -> Self {
        Self(value)
    }

    pub const fn into_raw(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreparedQuery {
    pub id: PreparedId,
    pub parameter_count: usize,
}

#[derive(Debug, Clone)]
struct PreparedStatement {
    sql: String,
    shape: QueryExpr,
    ddl_epoch: u64,
}

/// Connection-owned prepared-statement state. A registry must not be shared
/// between logical connections; IDs resolve only in the instance that minted them.
pub struct PreparedRegistry {
    statements: parking_lot::RwLock<BTreeMap<PreparedId, PreparedStatement>>,
    next_id: AtomicU64,
    enabled: AtomicBool,
    capacity: NonZeroUsize,
}

impl PreparedRegistry {
    /// Build a connection-scoped registry bounded to 1,024 entries. Once
    /// full, preparing a query evicts the oldest prepared ID.
    pub fn new() -> Self {
        Self::with_nonzero_capacity(
            NonZeroUsize::new(DEFAULT_PREPARED_CAPACITY)
                .expect("invariant: DEFAULT_PREPARED_CAPACITY is non-zero"),
        )
    }

    /// Build a connection-scoped registry with oldest-ID eviction at `capacity`.
    pub fn with_capacity(capacity: usize) -> RedDBResult<Self> {
        let capacity = NonZeroUsize::new(capacity).ok_or_else(|| {
            RedDBError::InvalidOperation(
                "prepared registry capacity must be greater than zero".to_string(),
            )
        })?;
        Ok(Self::with_nonzero_capacity(capacity))
    }

    fn with_nonzero_capacity(capacity: NonZeroUsize) -> Self {
        Self {
            statements: parking_lot::RwLock::new(BTreeMap::new()),
            next_id: AtomicU64::new(1),
            enabled: AtomicBool::new(!prepared_disabled_from_env()),
            capacity,
        }
    }

    pub fn prepare(&self, runtime: &RedDBRuntime, sql: &str) -> RedDBResult<PreparedQuery> {
        self.ensure_enabled()?;
        let shape = parse_multi(sql).map_err(|error| RedDBError::Query(error.to_string()))?;
        let indices = user_params::collect_indices(&shape);
        let parameter_count = indices.iter().copied().max().map_or(0, |index| index + 1);
        user_params::validate(&indices, parameter_count)
            .map_err(|error| RedDBError::Query(error.to_string()))?;
        let mut statements = self.statements.write();
        let id = PreparedId(self.next_id.fetch_add(1, Ordering::Relaxed));
        if statements.len() == self.capacity.get() {
            statements.pop_first();
        }
        statements.insert(
            id,
            PreparedStatement {
                sql: sql.to_string(),
                shape,
                ddl_epoch: runtime.ddl_epoch(),
            },
        );
        Ok(PreparedQuery {
            id,
            parameter_count,
        })
    }

    /// One-way emergency kill switch. Existing entries stay allocated but
    /// cannot be prepared or executed after this call.
    pub fn disable(&self) {
        self.enabled.store(false, Ordering::Release);
    }

    fn get(&self, id: PreparedId) -> RedDBResult<PreparedStatement> {
        self.ensure_enabled()?;
        self.statements
            .read()
            .get(&id)
            .cloned()
            .ok_or_else(|| RedDBError::Query("prepared statement not found or expired".to_string()))
    }

    fn ensure_enabled(&self) -> RedDBResult<()> {
        if self.enabled.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(RedDBError::Query(
                "prepared statements disabled".to_string(),
            ))
        }
    }
}

fn prepared_disabled_from_env() -> bool {
    std::env::var("REDDB_DISABLE_PREPARED")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "on" | "yes"
            )
        })
}

impl Default for PreparedRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub struct QueryRequestExecutor<'a> {
    runtime: &'a RedDBRuntime,
    prepared: &'a PreparedRegistry,
}

impl<'a> QueryRequestExecutor<'a> {
    pub fn new(runtime: &'a RedDBRuntime, prepared: &'a PreparedRegistry) -> Self {
        Self { runtime, prepared }
    }

    pub fn execute(&self, request: QueryRequest) -> RedDBResult<RuntimeQueryResult> {
        let commit_policy = request.commit_policy;
        self.runtime
            .resolve_request_commit_policy(commit_policy)
            .map_err(super::impl_replication_commit::commit_policy_violation_error)?;
        let params = request
            .params
            .into_iter()
            .map(Value::from)
            .collect::<Vec<_>>();
        let result = match request.source {
            QuerySource::Sql(sql) => self.runtime.execute_query_with_params(&sql, &params),
            QuerySource::Prepared(id) => {
                let prepared = self.prepared.get(id)?;
                if prepared.ddl_epoch != self.runtime.ddl_epoch() {
                    return Err(RedDBError::Query("prepared_needs_replan".to_string()));
                }
                let bound = user_params::bind(&prepared.shape, &params)
                    .map_err(|error| RedDBError::Query(error.to_string()))?;
                self.runtime.execute_prepared_query(&prepared.sql, bound)
            }
        }?;
        if matches!(result.statement_type, "insert" | "update" | "delete") {
            self.runtime
                .enforce_commit_policy_for_request(self.runtime.cdc_current_lsn(), commit_policy)?;
        }
        Ok(result)
    }
}
