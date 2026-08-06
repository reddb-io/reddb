use super::{RedDBRuntime, RuntimeQueryResult};
use crate::storage::query::ast::QueryExpr;
use crate::storage::query::modes::parse_multi;
use crate::storage::query::planner::shape::{bind_parameterized_query, parameterize_query_expr};
use crate::storage::query::user_params;
use reddb_types::Value;
use crate::{RedDBError, RedDBResult};
use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const DEFAULT_PREPARED_CAPACITY: usize = 1_024;

/// Transport-neutral parameter value vocabulary from ADR 0015.
///
/// `UInt64` extends the ADR's ten-variant enumeration with the wire
/// taxonomy's unsigned type (`WireValue::U64`, tag `VAL_U64`). The legacy
/// binary protocol binds unsigned parameters over the full u64 range and
/// as a distinct engine type; folding them into `Int64` would reject every
/// value above `i64::MAX` and change the bound value's type, so the seam
/// has to be able to say what the wire already says.
#[derive(Debug, Clone, PartialEq)]
pub enum ParamValue {
    Null,
    Bool(bool),
    Int64(i64),
    UInt64(u64),
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
            ParamValue::UInt64(value) => Self::UnsignedInteger(value),
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

/// How a prepared shape names its parameter slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParamBinding {
    /// The parser emitted one `Expr::Parameter` per client `$N` placeholder.
    UserPlaceholders,
    /// The planner's shape cache turned every literal into a parameter slot.
    /// This is what the legacy binary protocol has always prepared: `WHERE
    /// id = 5` reports one parameter in PREPARED_OK and clients bind against
    /// that count.
    AutoParameterizedLiterals,
}

#[derive(Debug, Clone)]
struct PreparedStatement {
    sql: String,
    shape: QueryExpr,
    parameter_count: usize,
    binding: ParamBinding,
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

    /// Prepare a statement whose parameters are the client's `$N`
    /// placeholders (ADR 0015).
    pub fn prepare(&self, runtime: &RedDBRuntime, sql: &str) -> RedDBResult<PreparedQuery> {
        self.ensure_enabled()?;
        let shape = parse_multi(sql).map_err(|error| RedDBError::Query(error.to_string()))?;
        let indices = user_params::collect_indices(&shape);
        let parameter_count = indices.iter().copied().max().map_or(0, |index| index + 1);
        user_params::validate(&indices, parameter_count)
            .map_err(|error| RedDBError::Query(error.to_string()))?;
        Ok(self.insert(
            runtime,
            sql,
            shape,
            parameter_count,
            ParamBinding::UserPlaceholders,
        ))
    }

    /// Prepare a statement whose parameters are the planner's
    /// auto-parameterized literals instead of `$N` placeholders.
    ///
    /// The legacy binary protocol reports `WHERE id = 5` as one parameter in
    /// PREPARED_OK and its clients bind positionally against that count, so
    /// the wire prepares through here. `prepare` stays the entry point for
    /// transports that speak ADR 0015 placeholders.
    pub fn prepare_auto_parameterized(
        &self,
        runtime: &RedDBRuntime,
        sql: &str,
    ) -> RedDBResult<PreparedQuery> {
        self.ensure_enabled()?;
        let parsed = parse_multi(sql).map_err(|error| RedDBError::Query(error.to_string()))?;
        // Runtime-side view rewrite runs at execute time, not prepare — view
        // bodies may change between the two on another thread, and rewriting
        // here would pin stale bodies into the shape.
        let (shape, parameter_count) = match parameterize_query_expr(&parsed) {
            Some(parameterized) => (parameterized.shape, parameterized.parameter_count),
            None => (parsed, 0),
        };
        Ok(self.insert(
            runtime,
            sql,
            shape,
            parameter_count,
            ParamBinding::AutoParameterizedLiterals,
        ))
    }

    fn insert(
        &self,
        runtime: &RedDBRuntime,
        sql: &str,
        shape: QueryExpr,
        parameter_count: usize,
        binding: ParamBinding,
    ) -> PreparedQuery {
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
                parameter_count,
                binding,
                ddl_epoch: runtime.ddl_epoch(),
            },
        );
        PreparedQuery {
            id,
            parameter_count,
        }
    }

    /// One-way emergency kill switch. Existing entries stay allocated but
    /// cannot be prepared or executed after this call.
    pub fn disable(&self) {
        self.enabled.store(false, Ordering::Release);
    }

    /// Whether prepared statements are still served on this connection.
    /// Transports gate their handlers on this so the kill switch keeps its
    /// own wire error text instead of inheriting an execution error's.
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    /// DDL epoch captured when `id` was prepared, or `None` when the entry is
    /// gone. Transports check it themselves to keep the wire's
    /// `prepared_needs_replan` precedence ahead of their arity check.
    pub fn ddl_epoch(&self, id: PreparedId) -> Option<u64> {
        self.statements
            .read()
            .get(&id)
            .map(|statement| statement.ddl_epoch)
    }

    /// Drop a prepared entry — DEALLOCATE frees the shape, not just the
    /// transport's wire-id mapping.
    pub fn release(&self, id: PreparedId) {
        self.statements.write().remove(&id);
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

/// `REDDB_DISABLE_PREPARED=1` forces clients onto the plain query path by
/// making every registry operation fail. Read once per process: registries are
/// per-connection and the binary listener builds one per text query, so this
/// must not cost an environment lookup on the hot path.
fn prepared_disabled_from_env() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| {
        std::env::var("REDDB_DISABLE_PREPARED")
            .ok()
            .is_some_and(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "on" | "yes"
                )
            })
    })
}

impl Default for PreparedRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
enum BoundStatement {
    Sql { sql: String, params: Vec<Value> },
    // Boxed: a bound `QueryExpr` dwarfs the text variant.
    Prepared { sql: String, expr: Box<QueryExpr> },
}

/// A request resolved to the statement that will run, before it runs.
///
/// Transports with a zero-copy fast path — the binary listener's direct scan —
/// need the bound `QueryExpr` to test scan eligibility. Handing it out here
/// keeps a single binder instead of forking one back into the transport.
#[derive(Debug, Clone)]
pub struct BoundRequest {
    statement: BoundStatement,
    commit_policy: Option<crate::replication::CommitPolicy>,
}

impl BoundRequest {
    /// The bound expression of a prepared request, or `None` for text SQL,
    /// which the runtime parses inside its own statement frame.
    pub fn prepared_expr(&self) -> Option<&QueryExpr> {
        match &self.statement {
            BoundStatement::Prepared { expr, .. } => Some(expr.as_ref()),
            BoundStatement::Sql { .. } => None,
        }
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

    /// Resolve a request to its bound statement without executing it.
    pub fn bind(&self, request: QueryRequest) -> RedDBResult<BoundRequest> {
        let commit_policy = request.commit_policy;
        let params = request
            .params
            .into_iter()
            .map(Value::from)
            .collect::<Vec<_>>();
        let statement = match request.source {
            QuerySource::Sql(sql) => {
                self.reject_weaker_commit_policy(&sql, commit_policy)?;
                BoundStatement::Sql { sql, params }
            }
            QuerySource::Prepared(id) => {
                let prepared = self.prepared.get(id)?;
                if prepared.ddl_epoch != self.runtime.ddl_epoch() {
                    return Err(RedDBError::Query("prepared_needs_replan".to_string()));
                }
                self.reject_weaker_commit_policy(&prepared.sql, commit_policy)?;
                let expr = match prepared.binding {
                    ParamBinding::UserPlaceholders => {
                        user_params::bind(&prepared.shape, &params)
                            .map_err(|error| RedDBError::Query(error.to_string()))?
                    }
                    // A shape with no slots is already executable. Statement
                    // kinds the planner does not auto-parameterize (DML, DDL,
                    // commands) land here, and the binder rejects those
                    // outright rather than returning them unchanged.
                    ParamBinding::AutoParameterizedLiterals if prepared.parameter_count == 0 => {
                        prepared.shape.clone()
                    }
                    ParamBinding::AutoParameterizedLiterals => {
                        bind_parameterized_query(&prepared.shape, &params, prepared.parameter_count)
                            .ok_or_else(|| RedDBError::Query("prepared bind failed".to_string()))?
                    }
                };
                BoundStatement::Prepared {
                    sql: prepared.sql,
                    expr: Box::new(expr),
                }
            }
        };
        Ok(BoundRequest {
            statement,
            commit_policy,
        })
    }

    /// Execute an already-bound request and enforce its commit policy.
    pub fn execute_bound(&self, bound: BoundRequest) -> RedDBResult<RuntimeQueryResult> {
        let commit_policy = bound.commit_policy;
        let result = match bound.statement {
            BoundStatement::Sql { sql, params } => {
                self.runtime.execute_query_with_params(&sql, &params)
            }
            BoundStatement::Prepared { sql, expr } => {
                self.runtime.execute_prepared_query(&sql, *expr)
            }
        }?;
        if matches!(result.statement_type, "insert" | "update" | "delete") {
            self.runtime
                .enforce_commit_policy_for_request(self.runtime.cdc_current_lsn(), commit_policy)?;
        }
        Ok(result)
    }

    pub fn execute(&self, request: QueryRequest) -> RedDBResult<RuntimeQueryResult> {
        self.execute_bound(self.bind(request)?)
    }

    /// Refuse a per-request policy weaker than the resolved floor before the
    /// write lands.
    ///
    /// Gated on mutating statements: a read never consulted the commit path
    /// before this seam existed, and must not start failing because a client
    /// attached a weak policy to a SELECT. Requests that carry no override
    /// resolve to the floor by definition, so they skip the classifier
    /// entirely and the hot text-query path stays free.
    fn reject_weaker_commit_policy(
        &self,
        sql: &str,
        commit_policy: Option<crate::replication::CommitPolicy>,
    ) -> RedDBResult<()> {
        if commit_policy.is_none() || !super::statement_frame::statement_is_write(sql) {
            return Ok(());
        }
        self.runtime
            .resolve_request_commit_policy(commit_policy)
            .map(|_| ())
            .map_err(super::impl_replication_commit::commit_policy_violation_error)
    }
}
