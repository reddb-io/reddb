use super::{RedDBRuntime, RuntimeQueryResult};
use crate::storage::query::planner::shape::{bind_parameterized_query, parameterize_query_expr};
use crate::storage::query::user_params;
use crate::{RedDBError, RedDBResult};
use reddb_rql::ast::QueryExpr;
use reddb_rql::modes::parse_multi;
use reddb_types::Value;
use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const DEFAULT_PREPARED_CAPACITY: usize = 1_024;

/// Transport-neutral parameter value vocabulary from ADR 0015.
///
/// `UInt64` and `DecimalText` extend the ADR's ten-variant enumeration with
/// the lossless wire/body number vocabulary. The legacy binary protocol
/// binds unsigned parameters over the full u64 range, while JSON transports
/// carry beyond-native precision in `$decimal` envelopes; the seam preserves
/// both instead of narrowing them before execution.
#[derive(Debug, Clone, PartialEq)]
pub enum ParamValue {
    Null,
    Bool(bool),
    Int64(i64),
    UInt64(u64),
    Float64(f64),
    DecimalText(String),
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
            ParamValue::DecimalText(value) => Self::DecimalText(value),
            ParamValue::Text(value) => Self::text(value),
            ParamValue::Bytes(value) => Self::Blob(value),
            ParamValue::Vector(value) => Self::Vector(value),
            ParamValue::Json(value) => Self::Json(value),
            ParamValue::Timestamp(value) => Self::Timestamp(value),
            ParamValue::Uuid(value) => Self::Uuid(value),
        }
    }
}

impl ParamValue {
    /// Decode the shared JSON parameter vocabulary used by HTTP, MCP, and
    /// stdio JSON-RPC. Typed single-key envelopes preserve values ordinary
    /// JSON cannot represent losslessly.
    pub fn decode_json(value: &crate::json::Value) -> Result<Self, String> {
        use crate::json::Value as JsonValue;

        match value {
            JsonValue::Null => Ok(Self::Null),
            JsonValue::Bool(value) => Ok(Self::Bool(*value)),
            JsonValue::Integer(value) => Ok(Self::Int64(*value)),
            JsonValue::Number(value) => {
                if value.is_finite()
                    && value.fract() == 0.0
                    && *value >= i64::MIN as f64
                    && *value <= i64::MAX as f64
                {
                    Ok(Self::Int64(*value as i64))
                } else {
                    Ok(Self::Float64(*value))
                }
            }
            JsonValue::Decimal(value) => Ok(Self::DecimalText(value.clone())),
            JsonValue::String(value) => Ok(Self::Text(value.clone())),
            JsonValue::Array(items) => {
                if items
                    .iter()
                    .all(|value| matches!(value, JsonValue::Integer(_) | JsonValue::Number(_)))
                {
                    Ok(Self::Vector(
                        items
                            .iter()
                            .map(|value| value.as_f64().unwrap_or(0.0) as f32)
                            .collect(),
                    ))
                } else {
                    Ok(Self::Json(crate::json::to_vec(value).unwrap_or_default()))
                }
            }
            JsonValue::Object(map) => {
                if map.len() == 1 {
                    if let Some(JsonValue::String(encoded)) = map.get("$bytes") {
                        if let Ok(bytes) = decode_base64(encoded) {
                            return Ok(Self::Bytes(bytes));
                        }
                    }
                    if let Some(value) = map.get("$ts") {
                        if let Some(timestamp) = json_i64(value) {
                            return Ok(Self::Timestamp(timestamp));
                        }
                    }
                    if let Some(JsonValue::String(value)) = map.get("$uuid") {
                        if let Ok(uuid) = crate::crypto::Uuid::parse_str(value) {
                            return Ok(Self::Uuid(*uuid.as_bytes()));
                        }
                    }
                    if let Some(JsonValue::String(encoded)) = map.get("$float") {
                        return Ok(match encoded.as_str() {
                            "NaN" => Self::Float64(f64::NAN),
                            "Infinity" | "+Infinity" | "inf" | "+inf" => {
                                Self::Float64(f64::INFINITY)
                            }
                            "-Infinity" | "-inf" => Self::Float64(f64::NEG_INFINITY),
                            _ => Self::Json(crate::json::to_vec(value).unwrap_or_default()),
                        });
                    }
                    if map.contains_key("$number") || map.contains_key("$decimalText") {
                        return Err("superseded exact-number envelope".to_string());
                    }
                    if let Some(value) = map.get("$int") {
                        return json_i64(value)
                            .map(Self::Int64)
                            .ok_or_else(|| "invalid $int exact-number envelope".to_string());
                    }
                    if let Some(value) = map.get("$uint") {
                        if let JsonValue::String(value) = value {
                            return value
                                .parse::<u64>()
                                .map(Self::UInt64)
                                .map_err(|_| "invalid $uint exact-number envelope".to_string());
                        }
                        return Err("invalid $uint exact-number envelope".to_string());
                    }
                    if let Some(JsonValue::String(value)) = map.get("$decimal") {
                        return Ok(Self::DecimalText(value.clone()));
                    }
                }
                Ok(Self::Json(crate::json::to_vec(value).unwrap_or_default()))
            }
        }
    }

    /// Decode a general JSON payload while preserving unrecognized typed
    /// envelopes as JSON. Row insertion historically accepts those objects
    /// as document values rather than rejecting the request as a bind error.
    pub fn decode_json_lossy(value: &crate::json::Value) -> Self {
        Self::decode_json(value)
            .unwrap_or_else(|_| Self::Json(crate::json::to_vec(value).unwrap_or_default()))
    }
}

fn json_i64(value: &crate::json::Value) -> Option<i64> {
    match value {
        crate::json::Value::Integer(value) => Some(*value),
        crate::json::Value::Number(value) => {
            if value.is_finite()
                && value.fract() == 0.0
                && *value >= i64::MIN as f64
                && *value <= i64::MAX as f64
            {
                Some(*value as i64)
            } else {
                None
            }
        }
        crate::json::Value::String(value) => value.parse::<i64>().ok(),
        _ => None,
    }
}

fn decode_base64(input: &str) -> Result<Vec<u8>, String> {
    let bytes = input.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return Err("base64 length must be a multiple of 4".to_string());
    }
    let mut output = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks_exact(4) {
        let padding = chunk.iter().rev().take_while(|&&byte| byte == b'=').count();
        let a = base64_value(chunk[0])?;
        let b = base64_value(chunk[1])?;
        let c = if chunk[2] == b'=' {
            0
        } else {
            base64_value(chunk[2])?
        };
        let d = if chunk[3] == b'=' {
            0
        } else {
            base64_value(chunk[3])?
        };
        let decoded = ((a as u32) << 18) | ((b as u32) << 12) | ((c as u32) << 6) | d as u32;
        output.push(((decoded >> 16) & 0xff) as u8);
        if padding < 2 {
            output.push(((decoded >> 8) & 0xff) as u8);
        }
        if padding < 1 {
            output.push((decoded & 0xff) as u8);
        }
    }
    Ok(output)
}

fn base64_value(byte: u8) -> Result<u8, String> {
    match byte {
        b'A'..=b'Z' => Ok(byte - b'A'),
        b'a'..=b'z' => Ok(byte - b'a' + 26),
        b'0'..=b'9' => Ok(byte - b'0' + 52),
        b'+' => Ok(62),
        b'/' => Ok(63),
        b'=' => Ok(0),
        _ => Err(format!("invalid base64 character: {}", byte as char)),
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
