//! IAM-style policy kernel: data model, JSON codec, validator, evaluator,
//! and simulator.
//!
//! This module is intentionally self-contained — it owns the *policy object*
//! and the *decision algorithm* but knows nothing about how policies are
//! stored, attached to principals, or fronted by HTTP. A separate
//! integration layer plumbs `Policy` through the auth store and surfaces
//! the simulator on the admin API.
//!
//! # Decision algorithm
//! `evaluate(policies, action, resource, ctx)` walks the supplied policy
//! list in order. The list is expected to be ordered "least specific
//! first": platform-level group attachments come first, tenant attachments
//! next, user attachments last. Within each policy, statements are
//! evaluated left-to-right.
//!
//! 1. If `ctx.principal_is_admin_role` is true, return `AdminBypass`
//!    immediately. This preserves the legacy 3-role escape hatch.
//! 2. For each statement, check the condition first (cheap), then the
//!    action set, then the resource set.
//! 3. Any matching `Deny` short-circuits to `Decision::Deny`.
//! 4. The first matching `Allow` is recorded but evaluation continues so
//!    that a later `Deny` can still override it.
//! 5. If no statement matched at all, return `DefaultDeny`.
//!
//! # Glob semantics
//! Globs are *split-on-`*`*: a pattern is broken into a prefix, a suffix,
//! and an ordered list of "contains" segments. Matching walks the input
//! checking that the prefix is at the start, the suffix is at the end,
//! and each contains segment appears in order. There is no regex engine
//! and no character classes — keep the matcher boring on purpose.
//!
//! # Time windows
//! `TimeWindow.tz_offset_secs` is a fixed signed offset from UTC. This
//! kernel intentionally does NOT depend on `chrono-tz` or any IANA tz
//! database (none is currently a dependency). The integration agent can
//! extend `TimeWindow` to accept IANA names later; until then, callers
//! must pass an explicit offset (`+HH:MM`/`-HH:MM`) or 0 for UTC.
//!
//! # Limits
//! - 100 statements per policy
//! - 50 actions per statement
//! - 50 resources per statement
//! - 32 KiB serialized JSON per policy

use std::error::Error;
use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;

use crate::serde_json::{self, JsonDecode, JsonEncode, Map, Value};

// ---------------------------------------------------------------------------
// Limits
// ---------------------------------------------------------------------------

/// Maximum statements per policy.
pub const MAX_STATEMENTS: usize = 100;
/// Maximum actions per statement.
pub const MAX_ACTIONS: usize = 50;
/// Maximum resources per statement.
pub const MAX_RESOURCES: usize = 50;
/// Maximum serialized JSON size in bytes.
pub const MAX_POLICY_BYTES: usize = 32 * 1024;

/// Recognised action verbs. Anything outside this allowlist is rejected
/// at validation time so a typo in a policy can never silently widen
/// access.
const ACTION_ALLOWLIST: &[&str] = &[
    "select",
    "insert",
    "update",
    "delete",
    "truncate",
    "references",
    "execute",
    "usage",
    "grant",
    "revoke",
    "create",
    "drop",
    "alter",
    "policy:put",
    "policy:drop",
    "policy:attach",
    "policy:detach",
    "policy:simulate",
    "admin:bootstrap",
    "admin:audit-read",
    "admin:reload",
    "admin:lease-promote",
    "*",
    "admin:*",
    "policy:*",
];

// ---------------------------------------------------------------------------
// Policy / Statement
// ---------------------------------------------------------------------------

/// A single IAM-style policy document.
#[derive(Debug, Clone, PartialEq)]
pub struct Policy {
    /// Unique policy id within a tenant.
    pub id: String,
    /// Schema version. Currently `1`.
    pub version: u8,
    pub statements: Vec<Statement>,
    /// `None` = platform-wide policy, `Some(t)` = tenant-scoped.
    pub tenant: Option<String>,
    /// Creation timestamp (unix ms).
    pub created_at: u128,
    /// Last-update timestamp (unix ms).
    pub updated_at: u128,
}

/// One Allow/Deny rule inside a policy.
#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    /// Optional human-readable id, unique within the policy.
    pub sid: Option<String>,
    pub effect: Effect,
    pub actions: Vec<ActionPattern>,
    pub resources: Vec<ResourcePattern>,
    pub condition: Option<Condition>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Allow,
    Deny,
}

/// Action match pattern.
///
/// `Prefix(s)` is stored *without* the trailing `:*` — `"admin:*"` parses
/// to `Prefix("admin")` so the matcher can compare against `admin:foo`
/// with a single `starts_with` + colon check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionPattern {
    Exact(String),
    Wildcard,
    Prefix(String),
}

/// Resource match pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourcePattern {
    Exact { kind: String, name: String },
    Glob(String),
    Wildcard,
}

/// Conditions that must hold for a statement to match. All present keys
/// are AND-combined; an absent key is "no constraint".
#[derive(Debug, Clone, PartialEq)]
pub struct Condition {
    pub expires_at: Option<u128>,
    pub valid_from: Option<u128>,
    pub tenant_match: Option<bool>,
    pub source_ip: Option<Vec<IpCidr>>,
    pub mfa: Option<bool>,
    pub time_window: Option<TimeWindow>,
}

/// CIDR block for `source_ip` matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpCidr {
    pub addr: IpAddr,
    pub prefix_len: u8,
}

/// Daily time window. Minutes are `HH * 60 + MM` in the local time zone
/// represented by `tz_offset_secs`. `from_minute > to_minute` means the
/// window wraps midnight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeWindow {
    pub from_minute: u16,
    pub to_minute: u16,
    pub tz_offset_secs: i32,
}

/// The resource being authorized — kind plus fully-qualified name.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceRef {
    pub kind: String,
    pub name: String,
    pub tenant: Option<String>,
}

impl ResourceRef {
    pub fn new(kind: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            name: name.into(),
            tenant: None,
        }
    }

    pub fn with_tenant(mut self, tenant: impl Into<String>) -> Self {
        self.tenant = Some(tenant.into());
        self
    }
}

/// Per-request evaluation context.
#[derive(Debug, Clone)]
pub struct EvalContext {
    /// Tenant of the authenticated principal.
    pub principal_tenant: Option<String>,
    /// Tenant the request is currently operating in (`SET TENANT`).
    pub current_tenant: Option<String>,
    /// Source IP of the connection (for `source_ip` conditions).
    pub peer_ip: Option<IpAddr>,
    pub mfa_present: bool,
    /// Wall clock at decision time (unix ms).
    pub now_ms: u128,
    /// Legacy 3-role bypass — set when the principal has the classic
    /// `Role::Admin`. Short-circuits the entire evaluator.
    pub principal_is_admin_role: bool,
}

impl Default for EvalContext {
    fn default() -> Self {
        Self {
            principal_tenant: None,
            current_tenant: None,
            peer_ip: None,
            mfa_present: false,
            now_ms: 0,
            principal_is_admin_role: false,
        }
    }
}

/// Outcome of `evaluate` / `simulate`.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Allow {
        matched_policy_id: String,
        matched_sid: Option<String>,
    },
    Deny {
        matched_policy_id: String,
        matched_sid: Option<String>,
    },
    DefaultDeny,
    AdminBypass,
}

// ---------------------------------------------------------------------------
// PolicyError
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum PolicyError {
    InvalidJson(String),
    InvalidAction(String),
    InvalidResource(String),
    InvalidCondition(String),
    InvalidCidr(String),
    DuplicateSid(String),
    EmptyStatements,
    EmptyActions,
    EmptyResources,
    TooManyStatements(usize),
    TooManyActions(usize),
    TooManyResources(usize),
    PolicyTooLarge(usize),
}

impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson(m) => write!(f, "invalid policy json: {m}"),
            Self::InvalidAction(m) => write!(f, "invalid action: {m}"),
            Self::InvalidResource(m) => write!(f, "invalid resource: {m}"),
            Self::InvalidCondition(m) => write!(f, "invalid condition: {m}"),
            Self::InvalidCidr(m) => write!(f, "invalid cidr: {m}"),
            Self::DuplicateSid(s) => write!(f, "duplicate sid in policy: {s}"),
            Self::EmptyStatements => write!(f, "policy has no statements"),
            Self::EmptyActions => write!(f, "statement has no actions"),
            Self::EmptyResources => write!(f, "statement has no resources"),
            Self::TooManyStatements(n) => {
                write!(f, "policy has {n} statements (max {MAX_STATEMENTS})")
            }
            Self::TooManyActions(n) => {
                write!(f, "statement has {n} actions (max {MAX_ACTIONS})")
            }
            Self::TooManyResources(n) => {
                write!(f, "statement has {n} resources (max {MAX_RESOURCES})")
            }
            Self::PolicyTooLarge(n) => {
                write!(f, "policy json is {n} bytes (max {MAX_POLICY_BYTES})")
            }
        }
    }
}

impl Error for PolicyError {}

// ---------------------------------------------------------------------------
// Policy: parse + validate + serialize
// ---------------------------------------------------------------------------

impl Policy {
    /// Parse and validate a policy from a JSON string. Enforces the 32 KiB
    /// size cap on the *raw* input before parsing.
    pub fn from_json_str(s: &str) -> Result<Policy, PolicyError> {
        if s.len() > MAX_POLICY_BYTES {
            return Err(PolicyError::PolicyTooLarge(s.len()));
        }
        let value: Value = serde_json::from_str(s).map_err(PolicyError::InvalidJson)?;
        let policy = Policy::from_json_value(&value)?;
        policy.validate()?;
        Ok(policy)
    }

    /// Serialize this policy to a compact JSON string. Round-trips with
    /// `from_json_str` modulo whitespace.
    pub fn to_json_string(&self) -> String {
        self.to_json_value().to_string_compact()
    }

    /// Validate structural invariants. Called automatically by
    /// `from_json_str` but also exposed for in-memory constructions.
    pub fn validate(&self) -> Result<(), PolicyError> {
        if self.statements.is_empty() {
            return Err(PolicyError::EmptyStatements);
        }
        if self.statements.len() > MAX_STATEMENTS {
            return Err(PolicyError::TooManyStatements(self.statements.len()));
        }

        let mut seen_sids: Vec<&str> = Vec::new();
        for st in &self.statements {
            if let Some(sid) = st.sid.as_deref() {
                if seen_sids.iter().any(|s| *s == sid) {
                    return Err(PolicyError::DuplicateSid(sid.to_string()));
                }
                seen_sids.push(sid);
            }
            if st.actions.is_empty() {
                return Err(PolicyError::EmptyActions);
            }
            if st.actions.len() > MAX_ACTIONS {
                return Err(PolicyError::TooManyActions(st.actions.len()));
            }
            if st.resources.is_empty() {
                return Err(PolicyError::EmptyResources);
            }
            if st.resources.len() > MAX_RESOURCES {
                return Err(PolicyError::TooManyResources(st.resources.len()));
            }
            for a in &st.actions {
                validate_action(a)?;
            }
        }
        Ok(())
    }

    fn from_json_value(v: &Value) -> Result<Policy, PolicyError> {
        let obj = v
            .as_object()
            .ok_or_else(|| PolicyError::InvalidJson("policy must be an object".into()))?;
        let id = string_field(obj, "id")?;
        let version = obj
            .get("version")
            .and_then(|n| n.as_u64())
            .map(|n| n as u8)
            .unwrap_or(1);
        let tenant = obj
            .get("tenant")
            .and_then(|t| match t {
                Value::Null => None,
                Value::String(s) => Some(Some(s.clone())),
                _ => Some(None),
            })
            .flatten();
        let created_at = parse_ts_field(obj, "created_at").unwrap_or(0);
        let updated_at = parse_ts_field(obj, "updated_at").unwrap_or(created_at);

        let statements_v =
            obj.get("statements")
                .and_then(|v| v.as_array())
                .ok_or(PolicyError::InvalidJson(
                    "policy.statements must be an array".into(),
                ))?;
        let mut statements = Vec::with_capacity(statements_v.len());
        for sv in statements_v {
            statements.push(Statement::from_json_value(sv)?);
        }

        Ok(Policy {
            id,
            version,
            statements,
            tenant,
            created_at,
            updated_at,
        })
    }

    fn to_json_value(&self) -> Value {
        let mut obj = Map::new();
        obj.insert("id".into(), Value::String(self.id.clone()));
        obj.insert("version".into(), Value::Number(self.version as f64));
        if let Some(t) = &self.tenant {
            obj.insert("tenant".into(), Value::String(t.clone()));
        } else {
            obj.insert("tenant".into(), Value::Null);
        }
        obj.insert("created_at".into(), Value::Number(self.created_at as f64));
        obj.insert("updated_at".into(), Value::Number(self.updated_at as f64));
        obj.insert(
            "statements".into(),
            Value::Array(self.statements.iter().map(|s| s.to_json_value()).collect()),
        );
        Value::Object(obj)
    }
}

impl JsonEncode for Policy {
    fn to_json_value(&self) -> Value {
        self.to_json_value()
    }
}

impl JsonDecode for Policy {
    fn from_json_value(value: Value) -> Result<Self, String> {
        Policy::from_json_value(&value).map_err(|e| e.to_string())
    }
}

// ---------------------------------------------------------------------------
// Statement parsing
// ---------------------------------------------------------------------------

impl Statement {
    fn from_json_value(v: &Value) -> Result<Statement, PolicyError> {
        let obj = v
            .as_object()
            .ok_or_else(|| PolicyError::InvalidJson("statement must be an object".into()))?;
        let sid = obj
            .get("sid")
            .and_then(|s| s.as_str())
            .map(|s| s.to_string());
        let effect_s = obj
            .get("effect")
            .and_then(|e| e.as_str())
            .ok_or_else(|| PolicyError::InvalidJson("statement.effect required".into()))?;
        let effect = match effect_s.to_ascii_lowercase().as_str() {
            "allow" => Effect::Allow,
            "deny" => Effect::Deny,
            other => return Err(PolicyError::InvalidJson(format!("unknown effect: {other}"))),
        };

        let actions = obj
            .get("actions")
            .and_then(|a| a.as_array())
            .ok_or_else(|| PolicyError::InvalidJson("statement.actions must be array".into()))?
            .iter()
            .map(|v| {
                v.as_str()
                    .ok_or_else(|| PolicyError::InvalidJson("action must be string".into()))
                    .map(compile_action)
            })
            .collect::<Result<Vec<_>, _>>()?;

        let resources = obj
            .get("resources")
            .and_then(|r| r.as_array())
            .ok_or_else(|| PolicyError::InvalidJson("statement.resources must be array".into()))?
            .iter()
            .map(|v| {
                v.as_str()
                    .ok_or_else(|| PolicyError::InvalidJson("resource must be string".into()))
                    .and_then(compile_resource)
            })
            .collect::<Result<Vec<_>, _>>()?;

        let condition = match obj.get("condition") {
            None | Some(Value::Null) => None,
            Some(c) => Some(Condition::from_json_value(c)?),
        };

        Ok(Statement {
            sid,
            effect,
            actions,
            resources,
            condition,
        })
    }

    fn to_json_value(&self) -> Value {
        let mut obj = Map::new();
        if let Some(sid) = &self.sid {
            obj.insert("sid".into(), Value::String(sid.clone()));
        }
        obj.insert(
            "effect".into(),
            Value::String(
                match self.effect {
                    Effect::Allow => "allow",
                    Effect::Deny => "deny",
                }
                .into(),
            ),
        );
        obj.insert(
            "actions".into(),
            Value::Array(
                self.actions
                    .iter()
                    .map(|a| Value::String(action_to_string(a)))
                    .collect(),
            ),
        );
        obj.insert(
            "resources".into(),
            Value::Array(
                self.resources
                    .iter()
                    .map(|r| Value::String(resource_to_string(r)))
                    .collect(),
            ),
        );
        if let Some(c) = &self.condition {
            obj.insert("condition".into(), c.to_json_value());
        }
        Value::Object(obj)
    }
}

// ---------------------------------------------------------------------------
// Condition parsing
// ---------------------------------------------------------------------------

impl Condition {
    fn from_json_value(v: &Value) -> Result<Condition, PolicyError> {
        let obj = v
            .as_object()
            .ok_or_else(|| PolicyError::InvalidCondition("condition must be object".into()))?;

        let expires_at = match obj.get("expires_at") {
            None | Some(Value::Null) => None,
            Some(x) => Some(parse_ts_value(x)?),
        };
        let valid_from = match obj.get("valid_from") {
            None | Some(Value::Null) => None,
            Some(x) => Some(parse_ts_value(x)?),
        };
        let tenant_match = obj.get("tenant_match").and_then(|v| v.as_bool());
        let mfa = obj.get("mfa").and_then(|v| v.as_bool());

        let source_ip = match obj.get("source_ip") {
            None | Some(Value::Null) => None,
            Some(arr) => {
                let xs = arr.as_array().ok_or_else(|| {
                    PolicyError::InvalidCondition("source_ip must be array".into())
                })?;
                let mut out = Vec::with_capacity(xs.len());
                for v in xs {
                    let s = v.as_str().ok_or_else(|| {
                        PolicyError::InvalidCidr("source_ip entry must be string".into())
                    })?;
                    out.push(parse_cidr(s)?);
                }
                Some(out)
            }
        };

        let time_window = match obj.get("time_window") {
            None | Some(Value::Null) => None,
            Some(tw) => Some(TimeWindow::from_json_value(tw)?),
        };

        Ok(Condition {
            expires_at,
            valid_from,
            tenant_match,
            source_ip,
            mfa,
            time_window,
        })
    }

    fn to_json_value(&self) -> Value {
        let mut obj = Map::new();
        if let Some(t) = self.expires_at {
            obj.insert("expires_at".into(), Value::Number(t as f64));
        }
        if let Some(t) = self.valid_from {
            obj.insert("valid_from".into(), Value::Number(t as f64));
        }
        if let Some(b) = self.tenant_match {
            obj.insert("tenant_match".into(), Value::Bool(b));
        }
        if let Some(b) = self.mfa {
            obj.insert("mfa".into(), Value::Bool(b));
        }
        if let Some(cidrs) = &self.source_ip {
            obj.insert(
                "source_ip".into(),
                Value::Array(
                    cidrs
                        .iter()
                        .map(|c| Value::String(format!("{}/{}", c.addr, c.prefix_len)))
                        .collect(),
                ),
            );
        }
        if let Some(tw) = &self.time_window {
            obj.insert("time_window".into(), tw.to_json_value());
        }
        Value::Object(obj)
    }
}

impl TimeWindow {
    fn from_json_value(v: &Value) -> Result<TimeWindow, PolicyError> {
        let obj = v
            .as_object()
            .ok_or_else(|| PolicyError::InvalidCondition("time_window must be object".into()))?;
        let from_minute =
            parse_hhmm(obj.get("from").and_then(|s| s.as_str()).ok_or_else(|| {
                PolicyError::InvalidCondition("time_window.from required".into())
            })?)?;
        let to_minute = parse_hhmm(
            obj.get("to")
                .and_then(|s| s.as_str())
                .ok_or_else(|| PolicyError::InvalidCondition("time_window.to required".into()))?,
        )?;
        let tz_str = obj.get("tz").and_then(|s| s.as_str()).unwrap_or("UTC");
        let tz_offset_secs = parse_tz_offset(tz_str)?;
        Ok(TimeWindow {
            from_minute,
            to_minute,
            tz_offset_secs,
        })
    }

    fn to_json_value(&self) -> Value {
        let mut obj = Map::new();
        obj.insert("from".into(), Value::String(format_hhmm(self.from_minute)));
        obj.insert("to".into(), Value::String(format_hhmm(self.to_minute)));
        obj.insert("tz".into(), Value::String(format_tz(self.tz_offset_secs)));
        Value::Object(obj)
    }
}

// ---------------------------------------------------------------------------
// Action / Resource helpers
// ---------------------------------------------------------------------------

/// Compile a string action into a pattern. `"*"` → wildcard, `"foo:*"` →
/// prefix-match on `foo`, anything else → exact match.
pub fn compile_action(s: &str) -> ActionPattern {
    if s == "*" {
        ActionPattern::Wildcard
    } else if let Some(p) = s.strip_suffix(":*") {
        ActionPattern::Prefix(p.to_string())
    } else {
        ActionPattern::Exact(s.to_string())
    }
}

fn action_to_string(a: &ActionPattern) -> String {
    match a {
        ActionPattern::Wildcard => "*".into(),
        ActionPattern::Prefix(p) => format!("{p}:*"),
        ActionPattern::Exact(s) => s.clone(),
    }
}

fn validate_action(a: &ActionPattern) -> Result<(), PolicyError> {
    let s = action_to_string(a);
    if ACTION_ALLOWLIST.iter().any(|w| *w == s) {
        Ok(())
    } else {
        Err(PolicyError::InvalidAction(s))
    }
}

fn compile_resource(s: &str) -> Result<ResourcePattern, PolicyError> {
    if s == "*" {
        return Ok(ResourcePattern::Wildcard);
    }
    if s.contains('*') {
        return Ok(ResourcePattern::Glob(s.to_string()));
    }
    let (kind, name) = s
        .split_once(':')
        .ok_or_else(|| PolicyError::InvalidResource(format!("expected `kind:name`, got `{s}`")))?;
    if kind.is_empty() || name.is_empty() {
        return Err(PolicyError::InvalidResource(s.to_string()));
    }
    Ok(ResourcePattern::Exact {
        kind: kind.to_string(),
        name: name.to_string(),
    })
}

fn resource_to_string(r: &ResourcePattern) -> String {
    match r {
        ResourcePattern::Wildcard => "*".into(),
        ResourcePattern::Exact { kind, name } => format!("{kind}:{name}"),
        ResourcePattern::Glob(s) => s.clone(),
    }
}

/// Compiled glob pattern: prefix + suffix + ordered "must contain"
/// segments (between consecutive `*` markers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledPattern {
    pub prefix: String,
    pub suffix: String,
    pub contains_segments: Vec<String>,
}

/// Split a `*`-glob into its compiled form. No regex involved.
pub fn compile_glob(pattern: &str) -> CompiledPattern {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        // No `*` at all — treat the whole pattern as a literal prefix
        // *and* suffix so plain equality still works through this matcher.
        return CompiledPattern {
            prefix: parts[0].to_string(),
            suffix: String::new(),
            contains_segments: Vec::new(),
        };
    }
    let prefix = parts[0].to_string();
    let suffix = parts[parts.len() - 1].to_string();
    let contains_segments = parts[1..parts.len() - 1]
        .iter()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    CompiledPattern {
        prefix,
        suffix,
        contains_segments,
    }
}

fn glob_matches(pat: &CompiledPattern, input: &str) -> bool {
    if !input.starts_with(&pat.prefix) {
        return false;
    }
    if !input.ends_with(&pat.suffix) {
        return false;
    }
    if pat.prefix.len() + pat.suffix.len() > input.len() {
        return false;
    }
    let mut cursor = pat.prefix.len();
    let inner_end = input.len() - pat.suffix.len();
    for seg in &pat.contains_segments {
        let hay = &input[cursor..inner_end];
        match hay.find(seg.as_str()) {
            Some(i) => cursor += i + seg.len(),
            None => return false,
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Timestamp + tz helpers
// ---------------------------------------------------------------------------

fn parse_ts_field(obj: &Map<String, Value>, key: &str) -> Option<u128> {
    obj.get(key).and_then(|v| parse_ts_value(v).ok())
}

fn parse_ts_value(v: &Value) -> Result<u128, PolicyError> {
    match v {
        Value::Number(n) if *n >= 0.0 => Ok(*n as u128),
        Value::String(s) => parse_rfc3339_ms(s),
        _ => Err(PolicyError::InvalidCondition(format!(
            "timestamp expected (rfc3339 or ms epoch), got {v:?}"
        ))),
    }
}

/// Parse a tiny RFC 3339 grammar — `YYYY-MM-DDTHH:MM:SS[.fff]Z` or with
/// `+HH:MM` / `-HH:MM` offsets. Pure stdlib: we convert to days-since-epoch
/// using the civil-from-days algorithm and then to milliseconds.
fn parse_rfc3339_ms(s: &str) -> Result<u128, PolicyError> {
    let bad = || PolicyError::InvalidCondition(format!("not rfc3339: {s}"));
    if s.len() < 20 {
        return Err(bad());
    }
    let bytes = s.as_bytes();
    if bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return Err(bad());
    }
    let year: i64 = s[0..4].parse().map_err(|_| bad())?;
    let month: u32 = s[5..7].parse().map_err(|_| bad())?;
    let day: u32 = s[8..10].parse().map_err(|_| bad())?;
    if bytes[13] != b':' || bytes[16] != b':' {
        return Err(bad());
    }
    let hour: u64 = s[11..13].parse().map_err(|_| bad())?;
    let minute: u64 = s[14..16].parse().map_err(|_| bad())?;
    let second: u64 = s[17..19].parse().map_err(|_| bad())?;

    // Optional fractional seconds.
    let mut idx = 19;
    let mut millis: u64 = 0;
    if idx < bytes.len() && bytes[idx] == b'.' {
        idx += 1;
        let start = idx;
        while idx < bytes.len() && bytes[idx].is_ascii_digit() {
            idx += 1;
        }
        let frac = &s[start..idx];
        if !frac.is_empty() {
            // Only the first three digits contribute to milliseconds.
            let take = frac.len().min(3);
            let pad = "0".repeat(3 - take);
            let combined = format!("{}{}", &frac[..take], pad);
            millis = combined.parse().map_err(|_| bad())?;
        }
    }

    // Trailing offset: `Z` or `±HH:MM`.
    let mut offset_secs: i64 = 0;
    if idx < bytes.len() {
        match bytes[idx] {
            b'Z' | b'z' => {
                idx += 1;
            }
            b'+' | b'-' => {
                if bytes.len() < idx + 6 || bytes[idx + 3] != b':' {
                    return Err(bad());
                }
                let sign: i64 = if bytes[idx] == b'+' { 1 } else { -1 };
                let oh: i64 = s[idx + 1..idx + 3].parse().map_err(|_| bad())?;
                let om: i64 = s[idx + 4..idx + 6].parse().map_err(|_| bad())?;
                offset_secs = sign * (oh * 3600 + om * 60);
                idx += 6;
            }
            _ => return Err(bad()),
        }
    }
    if idx != bytes.len() {
        return Err(bad());
    }

    let days = days_from_civil(year, month as i64, day as i64);
    let total_secs =
        days * 86_400 + (hour as i64) * 3600 + (minute as i64) * 60 + second as i64 - offset_secs;
    if total_secs < 0 {
        return Err(bad());
    }
    Ok((total_secs as u128) * 1000 + millis as u128)
}

/// Howard Hinnant's `days_from_civil` — converts a proleptic Gregorian
/// (Y, M, D) to days since 1970-01-01.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

fn parse_hhmm(s: &str) -> Result<u16, PolicyError> {
    let bad = || PolicyError::InvalidCondition(format!("HH:MM expected, got {s}"));
    if s.len() != 5 || s.as_bytes()[2] != b':' {
        return Err(bad());
    }
    let h: u16 = s[0..2].parse().map_err(|_| bad())?;
    let m: u16 = s[3..5].parse().map_err(|_| bad())?;
    if h >= 24 || m >= 60 {
        return Err(bad());
    }
    Ok(h * 60 + m)
}

fn format_hhmm(min: u16) -> String {
    format!("{:02}:{:02}", min / 60, min % 60)
}

fn parse_tz_offset(s: &str) -> Result<i32, PolicyError> {
    if s == "UTC" || s == "Z" {
        return Ok(0);
    }
    let bytes = s.as_bytes();
    if bytes.len() == 6 && (bytes[0] == b'+' || bytes[0] == b'-') && bytes[3] == b':' {
        let sign: i32 = if bytes[0] == b'+' { 1 } else { -1 };
        let h: i32 = s[1..3]
            .parse()
            .map_err(|_| PolicyError::InvalidCondition(format!("bad tz: {s}")))?;
        let m: i32 = s[4..6]
            .parse()
            .map_err(|_| PolicyError::InvalidCondition(format!("bad tz: {s}")))?;
        return Ok(sign * (h * 3600 + m * 60));
    }
    Err(PolicyError::InvalidCondition(format!(
        "tz must be UTC or +HH:MM/-HH:MM (got {s})"
    )))
}

fn format_tz(secs: i32) -> String {
    if secs == 0 {
        return "UTC".into();
    }
    let sign = if secs >= 0 { '+' } else { '-' };
    let abs = secs.abs();
    format!("{}{:02}:{:02}", sign, abs / 3600, (abs % 3600) / 60)
}

// ---------------------------------------------------------------------------
// CIDR helpers
// ---------------------------------------------------------------------------

fn parse_cidr(s: &str) -> Result<IpCidr, PolicyError> {
    let (addr_s, prefix_s) = match s.split_once('/') {
        Some(parts) => parts,
        None => {
            let addr =
                IpAddr::from_str(s).map_err(|e| PolicyError::InvalidCidr(format!("{s}: {e}")))?;
            let prefix_len = match addr {
                IpAddr::V4(_) => 32,
                IpAddr::V6(_) => 128,
            };
            return Ok(IpCidr { addr, prefix_len });
        }
    };
    let addr =
        IpAddr::from_str(addr_s).map_err(|e| PolicyError::InvalidCidr(format!("{s}: {e}")))?;
    let prefix_len: u8 = prefix_s
        .parse()
        .map_err(|_| PolicyError::InvalidCidr(format!("bad prefix in {s}")))?;
    let max = match addr {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    };
    if prefix_len > max {
        return Err(PolicyError::InvalidCidr(format!("prefix > {max} in {s}")));
    }
    Ok(IpCidr { addr, prefix_len })
}

fn cidr_contains(cidr: &IpCidr, ip: IpAddr) -> bool {
    match (cidr.addr, ip) {
        (IpAddr::V4(net), IpAddr::V4(ip)) => {
            let n = u32::from_be_bytes(net.octets());
            let i = u32::from_be_bytes(ip.octets());
            let mask = if cidr.prefix_len == 0 {
                0u32
            } else {
                u32::MAX << (32 - cidr.prefix_len)
            };
            (n & mask) == (i & mask)
        }
        (IpAddr::V6(net), IpAddr::V6(ip)) => {
            let n = u128::from_be_bytes(net.octets());
            let i = u128::from_be_bytes(ip.octets());
            let mask = if cidr.prefix_len == 0 {
                0u128
            } else {
                u128::MAX << (128 - cidr.prefix_len)
            };
            (n & mask) == (i & mask)
        }
        _ => false, // v4 vs v6 never match
    }
}

// ---------------------------------------------------------------------------
// Action / resource matching
// ---------------------------------------------------------------------------

fn action_matches(pat: &ActionPattern, action: &str) -> bool {
    match pat {
        ActionPattern::Wildcard => true,
        ActionPattern::Exact(s) => s == action,
        ActionPattern::Prefix(p) => {
            // `admin:*` matches `admin:foo` but not `admin` and not `administer`.
            action.len() > p.len() + 1
                && action.starts_with(p.as_str())
                && action.as_bytes()[p.len()] == b':'
        }
    }
}

/// Match a resource pattern against a concrete resource. Patterns that
/// don't include a tenant prefix (`tenant/...`) are implicitly scoped to
/// `ctx.current_tenant` so a policy author can write `table:public.foo`
/// without manually qualifying the tenant.
fn resource_matches(pat: &ResourcePattern, resource: &ResourceRef, ctx: &EvalContext) -> bool {
    let target = qualified_name(&resource.kind, &resource.name, resource.tenant.as_deref());
    match pat {
        ResourcePattern::Wildcard => true,
        ResourcePattern::Exact { kind, name } => {
            if kind != &resource.kind {
                return false;
            }
            let qualified = if name.starts_with("tenant/") {
                format!("{kind}:{name}")
            } else {
                qualified_name(kind, name, ctx.current_tenant.as_deref())
            };
            qualified == target
        }
        ResourcePattern::Glob(raw) => {
            let (pkind, pname) = match raw.split_once(':') {
                Some(parts) => parts,
                None => return false,
            };
            if !pkind.is_empty() && pkind != "*" && pkind != resource.kind {
                return false;
            }
            let qualified_pat = if pname.starts_with("tenant/") || pname == "*" {
                format!("{pkind}:{pname}")
            } else {
                let scoped = match ctx.current_tenant.as_deref() {
                    Some(t) => format!("tenant/{t}/{pname}"),
                    None => pname.to_string(),
                };
                format!("{pkind}:{scoped}")
            };
            let compiled = compile_glob(&qualified_pat);
            glob_matches(&compiled, &target)
        }
    }
}

/// Build the canonical fully-qualified resource name. `tenant/<t>/...` is
/// prepended when a tenant is in scope; platform resources stay bare.
fn qualified_name(kind: &str, name: &str, tenant: Option<&str>) -> String {
    if name.starts_with("tenant/") {
        return format!("{kind}:{name}");
    }
    match tenant {
        Some(t) => format!("{kind}:tenant/{t}/{name}"),
        None => format!("{kind}:{name}"),
    }
}

// ---------------------------------------------------------------------------
// Condition evaluator
// ---------------------------------------------------------------------------

fn condition_holds(cond: Option<&Condition>, resource: &ResourceRef, ctx: &EvalContext) -> bool {
    let Some(c) = cond else { return true };
    if let Some(exp) = c.expires_at {
        if ctx.now_ms >= exp {
            return false;
        }
    }
    if let Some(vf) = c.valid_from {
        if ctx.now_ms < vf {
            return false;
        }
    }
    if let Some(true) = c.tenant_match {
        if resource.tenant.as_deref() != ctx.current_tenant.as_deref() {
            return false;
        }
    }
    if let Some(true) = c.mfa {
        if !ctx.mfa_present {
            return false;
        }
    }
    if let Some(cidrs) = &c.source_ip {
        let Some(ip) = ctx.peer_ip else {
            return false;
        };
        if !cidrs.iter().any(|c| cidr_contains(c, ip)) {
            return false;
        }
    }
    if let Some(tw) = &c.time_window {
        if !time_window_contains(tw, ctx.now_ms) {
            return false;
        }
    }
    true
}

fn time_window_contains(tw: &TimeWindow, now_ms: u128) -> bool {
    // Convert ms-since-epoch to local minute-of-day.
    let now_secs = (now_ms / 1000) as i128 + tw.tz_offset_secs as i128;
    let day_secs = now_secs.rem_euclid(86_400);
    let minute = (day_secs / 60) as u16;
    if tw.from_minute <= tw.to_minute {
        minute >= tw.from_minute && minute <= tw.to_minute
    } else {
        // Wrap-around window: e.g. 22:00 .. 06:00
        minute >= tw.from_minute || minute <= tw.to_minute
    }
}

// ---------------------------------------------------------------------------
// Evaluator + simulator
// ---------------------------------------------------------------------------

/// Evaluate a request against an ordered list of policies. See the
/// module-level docs for the algorithm.
pub fn evaluate(
    policies: &[&Policy],
    action: &str,
    resource: &ResourceRef,
    ctx: &EvalContext,
) -> Decision {
    if ctx.principal_is_admin_role {
        return Decision::AdminBypass;
    }

    let mut allow_hit: Option<(String, Option<String>)> = None;

    for p in policies {
        for st in &p.statements {
            if !condition_holds(st.condition.as_ref(), resource, ctx) {
                continue;
            }
            if !st.actions.iter().any(|a| action_matches(a, action)) {
                continue;
            }
            if !st
                .resources
                .iter()
                .any(|r| resource_matches(r, resource, ctx))
            {
                continue;
            }
            match st.effect {
                Effect::Deny => {
                    return Decision::Deny {
                        matched_policy_id: p.id.clone(),
                        matched_sid: st.sid.clone(),
                    };
                }
                Effect::Allow => {
                    if allow_hit.is_none() {
                        allow_hit = Some((p.id.clone(), st.sid.clone()));
                    }
                }
            }
        }
    }

    match allow_hit {
        Some((pid, sid)) => Decision::Allow {
            matched_policy_id: pid,
            matched_sid: sid,
        },
        None => Decision::DefaultDeny,
    }
}

/// One row of a simulator trail.
#[derive(Debug, Clone, PartialEq)]
pub struct TrailEntry {
    pub policy_id: String,
    pub sid: Option<String>,
    pub matched: bool,
    pub effect: Effect,
    pub why_skipped: Option<&'static str>,
}

/// Simulator output — a `Decision` plus a human-readable trail.
#[derive(Debug, Clone, PartialEq)]
pub struct SimulationOutcome {
    pub decision: Decision,
    pub reason: String,
    pub trail: Vec<TrailEntry>,
}

/// Like `evaluate` but records every visited statement and produces a
/// human-readable explanation. Returns the same decision the evaluator
/// would have returned.
pub fn simulate(
    policies: &[&Policy],
    action: &str,
    resource: &ResourceRef,
    ctx: &EvalContext,
) -> SimulationOutcome {
    if ctx.principal_is_admin_role {
        return SimulationOutcome {
            decision: Decision::AdminBypass,
            reason: "admin bypass: principal has legacy Role::Admin".into(),
            trail: Vec::new(),
        };
    }

    let mut trail = Vec::new();
    let mut allow_hit: Option<(String, Option<String>, usize)> = None;
    let mut deny_hit: Option<(String, Option<String>, usize)> = None;

    'outer: for p in policies {
        for (idx, st) in p.statements.iter().enumerate() {
            let mut why: Option<&'static str> = None;
            let mut matched = false;

            if !condition_holds(st.condition.as_ref(), resource, ctx) {
                why = Some("condition not met");
            } else if !st.actions.iter().any(|a| action_matches(a, action)) {
                why = Some("no action match");
            } else if !st
                .resources
                .iter()
                .any(|r| resource_matches(r, resource, ctx))
            {
                why = Some("no resource match");
            } else {
                matched = true;
            }

            trail.push(TrailEntry {
                policy_id: p.id.clone(),
                sid: st.sid.clone(),
                matched,
                effect: st.effect,
                why_skipped: why,
            });

            if matched {
                match st.effect {
                    Effect::Deny => {
                        deny_hit = Some((p.id.clone(), st.sid.clone(), idx));
                        break 'outer;
                    }
                    Effect::Allow => {
                        if allow_hit.is_none() {
                            allow_hit = Some((p.id.clone(), st.sid.clone(), idx));
                        }
                    }
                }
            }
        }
    }

    if let Some((pid, sid, idx)) = deny_hit {
        let reason = format!(
            "deny at {}.statement[{}]{}",
            pid,
            idx,
            sid.as_ref()
                .map(|s| format!(" (sid={s})"))
                .unwrap_or_default()
        );
        return SimulationOutcome {
            decision: Decision::Deny {
                matched_policy_id: pid,
                matched_sid: sid,
            },
            reason,
            trail,
        };
    }
    if let Some((pid, sid, idx)) = allow_hit {
        let reason = format!(
            "allow at {}.statement[{}]{}",
            pid,
            idx,
            sid.as_ref()
                .map(|s| format!(" (sid={s})"))
                .unwrap_or_default()
        );
        return SimulationOutcome {
            decision: Decision::Allow {
                matched_policy_id: pid,
                matched_sid: sid,
            },
            reason,
            trail,
        };
    }
    SimulationOutcome {
        decision: Decision::DefaultDeny,
        reason: "no statement matched (default deny)".into(),
        trail,
    }
}

// ---------------------------------------------------------------------------
// JSON helpers
// ---------------------------------------------------------------------------

fn string_field(obj: &Map<String, Value>, key: &str) -> Result<String, PolicyError> {
    obj.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| PolicyError::InvalidJson(format!("policy.{key} required string")))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_policy_json() -> &'static str {
        r#"{
            "id": "p-min",
            "version": 1,
            "statements": [
                { "effect": "allow", "actions": ["select"], "resources": ["table:public.x"] }
            ]
        }"#
    }

    fn full_policy_json() -> &'static str {
        r#"{
            "id": "p-full",
            "version": 1,
            "tenant": "acme",
            "created_at": 1700000000000,
            "updated_at": 1700000001000,
            "statements": [
                {
                    "sid": "s1",
                    "effect": "allow",
                    "actions": ["select", "insert"],
                    "resources": ["table:public.orders", "table:public.*"]
                },
                {
                    "sid": "s2",
                    "effect": "deny",
                    "actions": ["delete"],
                    "resources": ["*"]
                }
            ]
        }"#
    }

    fn cond_policy_json() -> &'static str {
        r#"{
            "id": "p-cond",
            "version": 1,
            "statements": [
                {
                    "sid": "biz-hours",
                    "effect": "allow",
                    "actions": ["select"],
                    "resources": ["table:public.orders"],
                    "condition": {
                        "expires_at": "2099-12-31T23:59:59Z",
                        "valid_from": 1700000000000,
                        "tenant_match": true,
                        "source_ip": ["10.0.0.0/8"],
                        "mfa": true,
                        "time_window": { "from": "09:00", "to": "17:00", "tz": "UTC" }
                    }
                }
            ]
        }"#
    }

    fn ctx_now(now_ms: u128) -> EvalContext {
        EvalContext {
            now_ms,
            ..Default::default()
        }
    }

    // -----------------------------------------------------------------
    // JSON roundtrip
    // -----------------------------------------------------------------

    #[test]
    fn roundtrip_minimal() {
        let p = Policy::from_json_str(minimal_policy_json()).unwrap();
        let s = p.to_json_string();
        let p2 = Policy::from_json_str(&s).unwrap();
        assert_eq!(p, p2);
        assert_eq!(p.id, "p-min");
        assert_eq!(p.statements.len(), 1);
    }

    #[test]
    fn roundtrip_full() {
        let p = Policy::from_json_str(full_policy_json()).unwrap();
        let s = p.to_json_string();
        let p2 = Policy::from_json_str(&s).unwrap();
        assert_eq!(p, p2);
        assert_eq!(p.tenant.as_deref(), Some("acme"));
        assert_eq!(p.statements.len(), 2);
    }

    #[test]
    fn roundtrip_with_conditions() {
        let p = Policy::from_json_str(cond_policy_json()).unwrap();
        let s = p.to_json_string();
        let p2 = Policy::from_json_str(&s).unwrap();
        assert_eq!(p, p2);
        let c = p.statements[0].condition.as_ref().unwrap();
        assert!(c.expires_at.is_some());
        assert!(c.valid_from.is_some());
        assert_eq!(c.tenant_match, Some(true));
        assert_eq!(c.mfa, Some(true));
        let cidrs = c.source_ip.as_ref().unwrap();
        assert_eq!(cidrs.len(), 1);
        assert_eq!(cidrs[0].prefix_len, 8);
    }

    // -----------------------------------------------------------------
    // Validator rejection classes
    // -----------------------------------------------------------------

    #[test]
    fn validator_rejects_invalid_json() {
        let err = Policy::from_json_str("{ not json").unwrap_err();
        matches!(err, PolicyError::InvalidJson(_));
    }

    #[test]
    fn validator_rejects_invalid_action() {
        let bad = r#"{
            "id":"p","version":1,"statements":[
                {"effect":"allow","actions":["bogus"],"resources":["table:public.x"]}
            ]}"#;
        let err = Policy::from_json_str(bad).unwrap_err();
        assert!(matches!(err, PolicyError::InvalidAction(_)));
    }

    #[test]
    fn validator_rejects_invalid_resource() {
        let bad = r#"{
            "id":"p","version":1,"statements":[
                {"effect":"allow","actions":["select"],"resources":["nokind"]}
            ]}"#;
        let err = Policy::from_json_str(bad).unwrap_err();
        assert!(matches!(err, PolicyError::InvalidResource(_)));
    }

    #[test]
    fn validator_rejects_invalid_condition() {
        let bad = r#"{
            "id":"p","version":1,"statements":[
                {"effect":"allow","actions":["select"],"resources":["table:public.x"],
                 "condition":{"expires_at":{}}}
            ]}"#;
        let err = Policy::from_json_str(bad).unwrap_err();
        assert!(matches!(err, PolicyError::InvalidCondition(_)));
    }

    #[test]
    fn validator_rejects_invalid_cidr() {
        let bad = r#"{
            "id":"p","version":1,"statements":[
                {"effect":"allow","actions":["select"],"resources":["table:public.x"],
                 "condition":{"source_ip":["10.0.0.0/99"]}}
            ]}"#;
        let err = Policy::from_json_str(bad).unwrap_err();
        assert!(matches!(err, PolicyError::InvalidCidr(_)));
    }

    #[test]
    fn validator_rejects_duplicate_sid() {
        let bad = r#"{
            "id":"p","version":1,"statements":[
                {"sid":"x","effect":"allow","actions":["select"],"resources":["table:public.x"]},
                {"sid":"x","effect":"deny","actions":["delete"],"resources":["table:public.y"]}
            ]}"#;
        let err = Policy::from_json_str(bad).unwrap_err();
        assert!(matches!(err, PolicyError::DuplicateSid(_)));
    }

    #[test]
    fn validator_rejects_empty_statements() {
        let bad = r#"{"id":"p","version":1,"statements":[]}"#;
        let err = Policy::from_json_str(bad).unwrap_err();
        assert!(matches!(err, PolicyError::EmptyStatements));
    }

    #[test]
    fn validator_rejects_empty_actions() {
        let bad = r#"{
            "id":"p","version":1,"statements":[
                {"effect":"allow","actions":[],"resources":["table:public.x"]}
            ]}"#;
        let err = Policy::from_json_str(bad).unwrap_err();
        assert!(matches!(err, PolicyError::EmptyActions));
    }

    #[test]
    fn validator_rejects_empty_resources() {
        let bad = r#"{
            "id":"p","version":1,"statements":[
                {"effect":"allow","actions":["select"],"resources":[]}
            ]}"#;
        let err = Policy::from_json_str(bad).unwrap_err();
        assert!(matches!(err, PolicyError::EmptyResources));
    }

    #[test]
    fn validator_rejects_too_many_statements() {
        let mut p = Policy::from_json_str(minimal_policy_json()).unwrap();
        let st = p.statements[0].clone();
        for _ in 0..MAX_STATEMENTS {
            p.statements.push(st.clone());
        }
        let err = p.validate().unwrap_err();
        assert!(matches!(err, PolicyError::TooManyStatements(_)));
    }

    #[test]
    fn validator_rejects_too_many_actions() {
        let mut p = Policy::from_json_str(minimal_policy_json()).unwrap();
        for _ in 0..MAX_ACTIONS {
            p.statements[0].actions.push(ActionPattern::Wildcard);
        }
        let err = p.validate().unwrap_err();
        assert!(matches!(err, PolicyError::TooManyActions(_)));
    }

    #[test]
    fn validator_rejects_too_many_resources() {
        let mut p = Policy::from_json_str(minimal_policy_json()).unwrap();
        for _ in 0..MAX_RESOURCES {
            p.statements[0].resources.push(ResourcePattern::Wildcard);
        }
        let err = p.validate().unwrap_err();
        assert!(matches!(err, PolicyError::TooManyResources(_)));
    }

    #[test]
    fn validator_rejects_oversize_json() {
        let big = "x".repeat(MAX_POLICY_BYTES + 1);
        let err = Policy::from_json_str(&big).unwrap_err();
        assert!(matches!(err, PolicyError::PolicyTooLarge(_)));
    }

    // -----------------------------------------------------------------
    // Glob + action match
    // -----------------------------------------------------------------

    #[test]
    fn glob_matches_table_public_star() {
        let pat = compile_glob("table:public.*");
        assert!(glob_matches(&pat, "table:public.orders"));
        assert!(glob_matches(&pat, "table:public."));
        assert!(!glob_matches(&pat, "table:other.x"));
    }

    #[test]
    fn glob_matches_tenant_star() {
        let pat = compile_glob("tenant:acme/*");
        assert!(glob_matches(&pat, "tenant:acme/whatever"));
        assert!(glob_matches(&pat, "tenant:acme/a/b/c"));
        assert!(!glob_matches(&pat, "tenant:other/whatever"));
    }

    #[test]
    fn action_match_exact() {
        assert!(action_matches(&compile_action("select"), "select"));
        assert!(!action_matches(&compile_action("select"), "selectall"));
        assert!(!action_matches(&compile_action("select"), "insert"));
    }

    #[test]
    fn action_match_prefix() {
        let p = compile_action("admin:*");
        assert!(action_matches(&p, "admin:bootstrap"));
        assert!(action_matches(&p, "admin:reload"));
        assert!(!action_matches(&p, "admin"));
        assert!(!action_matches(&p, "select"));
    }

    #[test]
    fn action_match_wildcard() {
        let p = compile_action("*");
        assert!(action_matches(&p, "select"));
        assert!(action_matches(&p, "admin:bootstrap"));
        assert!(action_matches(&p, "policy:put"));
    }

    // -----------------------------------------------------------------
    // Conditions
    // -----------------------------------------------------------------

    #[test]
    fn condition_expires_at() {
        let c = Condition {
            expires_at: Some(2_000),
            valid_from: None,
            tenant_match: None,
            source_ip: None,
            mfa: None,
            time_window: None,
        };
        let r = ResourceRef::new("table", "x");
        assert!(condition_holds(Some(&c), &r, &ctx_now(1_000)));
        assert!(!condition_holds(Some(&c), &r, &ctx_now(2_000)));
        assert!(!condition_holds(Some(&c), &r, &ctx_now(2_500)));
    }

    #[test]
    fn condition_valid_from() {
        let c = Condition {
            expires_at: None,
            valid_from: Some(2_000),
            tenant_match: None,
            source_ip: None,
            mfa: None,
            time_window: None,
        };
        let r = ResourceRef::new("table", "x");
        assert!(!condition_holds(Some(&c), &r, &ctx_now(1_999)));
        assert!(condition_holds(Some(&c), &r, &ctx_now(2_000)));
        assert!(condition_holds(Some(&c), &r, &ctx_now(3_000)));
    }

    #[test]
    fn condition_source_ip_v4() {
        let c = Condition {
            expires_at: None,
            valid_from: None,
            tenant_match: None,
            source_ip: Some(vec![parse_cidr("10.0.0.0/8").unwrap()]),
            mfa: None,
            time_window: None,
        };
        let r = ResourceRef::new("table", "x");
        let mut ctx = ctx_now(1);
        ctx.peer_ip = Some(IpAddr::from_str("10.0.0.1").unwrap());
        assert!(condition_holds(Some(&c), &r, &ctx));
        ctx.peer_ip = Some(IpAddr::from_str("11.0.0.1").unwrap());
        assert!(!condition_holds(Some(&c), &r, &ctx));
        ctx.peer_ip = None;
        assert!(!condition_holds(Some(&c), &r, &ctx));
    }

    #[test]
    fn condition_source_ip_accepts_single_ip() {
        let cidr = parse_cidr("192.168.1.5").unwrap();
        assert_eq!(cidr.prefix_len, 32);

        let c = Condition {
            expires_at: None,
            valid_from: None,
            tenant_match: None,
            source_ip: Some(vec![cidr]),
            mfa: None,
            time_window: None,
        };
        let r = ResourceRef::new("table", "public.x");
        let mut ctx = ctx_now(1);
        ctx.peer_ip = Some(IpAddr::from_str("192.168.1.5").unwrap());
        assert!(condition_holds(Some(&c), &r, &ctx));
        ctx.peer_ip = Some(IpAddr::from_str("192.168.1.6").unwrap());
        assert!(!condition_holds(Some(&c), &r, &ctx));
    }

    #[test]
    fn condition_tenant_match() {
        let c = Condition {
            expires_at: None,
            valid_from: None,
            tenant_match: Some(true),
            source_ip: None,
            mfa: None,
            time_window: None,
        };
        let r = ResourceRef::new("table", "x").with_tenant("acme");
        let mut ctx = ctx_now(1);
        ctx.current_tenant = Some("acme".into());
        assert!(condition_holds(Some(&c), &r, &ctx));
        ctx.current_tenant = Some("globex".into());
        assert!(!condition_holds(Some(&c), &r, &ctx));
    }

    #[test]
    fn condition_mfa() {
        let c = Condition {
            expires_at: None,
            valid_from: None,
            tenant_match: None,
            source_ip: None,
            mfa: Some(true),
            time_window: None,
        };
        let r = ResourceRef::new("table", "x");
        let mut ctx = ctx_now(1);
        ctx.mfa_present = true;
        assert!(condition_holds(Some(&c), &r, &ctx));
        ctx.mfa_present = false;
        assert!(!condition_holds(Some(&c), &r, &ctx));
    }

    #[test]
    fn condition_time_window_normal() {
        // 09:00 .. 17:00 UTC. now = 1970-01-01T12:00:00Z = 12 * 3600 * 1000 ms.
        let tw = TimeWindow {
            from_minute: 9 * 60,
            to_minute: 17 * 60,
            tz_offset_secs: 0,
        };
        assert!(time_window_contains(&tw, 12 * 3_600_000));
        assert!(time_window_contains(&tw, 9 * 3_600_000));
        assert!(time_window_contains(&tw, 17 * 3_600_000));
        // 18:00 outside.
        assert!(!time_window_contains(&tw, 18 * 3_600_000));
        // 06:00 outside.
        assert!(!time_window_contains(&tw, 6 * 3_600_000));
    }

    #[test]
    fn condition_time_window_wraparound() {
        // 22:00 .. 06:00 UTC.
        let tw = TimeWindow {
            from_minute: 22 * 60,
            to_minute: 6 * 60,
            tz_offset_secs: 0,
        };
        assert!(time_window_contains(&tw, 23 * 3_600_000));
        assert!(time_window_contains(&tw, 1 * 3_600_000));
        assert!(time_window_contains(&tw, 6 * 3_600_000));
        assert!(!time_window_contains(&tw, 12 * 3_600_000));
        assert!(!time_window_contains(&tw, 21 * 3_600_000));
    }

    // -----------------------------------------------------------------
    // Evaluator
    // -----------------------------------------------------------------

    fn analyst_policy() -> Policy {
        Policy::from_json_str(
            r#"{
                "id":"analyst","version":1,"statements":[
                    {"sid":"reads","effect":"allow",
                     "actions":["select"],"resources":["table:public.orders"]}
                ]}"#,
        )
        .unwrap()
    }

    fn no_deletes_policy() -> Policy {
        Policy::from_json_str(
            r#"{
                "id":"no-deletes","version":1,"statements":[
                    {"sid":"hard-stop","effect":"deny",
                     "actions":["delete"],"resources":["*"]}
                ]}"#,
        )
        .unwrap()
    }

    #[test]
    fn evaluator_pure_allow() {
        let p = analyst_policy();
        let r = ResourceRef::new("table", "public.orders");
        let d = evaluate(&[&p], "select", &r, &EvalContext::default());
        match d {
            Decision::Allow {
                matched_policy_id,
                matched_sid,
            } => {
                assert_eq!(matched_policy_id, "analyst");
                assert_eq!(matched_sid.as_deref(), Some("reads"));
            }
            other => panic!("expected Allow, got {other:?}"),
        }
    }

    #[test]
    fn evaluator_deny_overrides_allow() {
        let allow = analyst_policy();
        let deny = no_deletes_policy();
        let r = ResourceRef::new("table", "public.orders");
        // Allow says nothing about delete; deny matches.
        let d = evaluate(&[&allow, &deny], "delete", &r, &EvalContext::default());
        match d {
            Decision::Deny {
                matched_policy_id, ..
            } => {
                assert_eq!(matched_policy_id, "no-deletes");
            }
            other => panic!("expected Deny, got {other:?}"),
        }
    }

    #[test]
    fn evaluator_default_deny() {
        let p = analyst_policy();
        let r = ResourceRef::new("table", "public.invoices");
        let d = evaluate(&[&p], "select", &r, &EvalContext::default());
        assert_eq!(d, Decision::DefaultDeny);
    }

    #[test]
    fn evaluator_admin_bypass() {
        let p = analyst_policy();
        let r = ResourceRef::new("table", "anything");
        let mut ctx = EvalContext::default();
        ctx.principal_is_admin_role = true;
        let d = evaluate(&[&p], "delete", &r, &ctx);
        assert_eq!(d, Decision::AdminBypass);
    }

    #[test]
    fn evaluator_implicit_tenant_scoping() {
        // Pattern `table:public.x` written without tenant prefix should
        // implicitly bind to ctx.current_tenant — so a request against
        // tenant `acme` matches but a request against tenant `globex`
        // does not (when the policy is evaluated with current_tenant=acme).
        let p = Policy::from_json_str(
            r#"{
                "id":"impl","version":1,"statements":[
                    {"sid":"s","effect":"allow",
                     "actions":["select"],"resources":["table:public.x"]}
                ]}"#,
        )
        .unwrap();
        let r_acme = ResourceRef::new("table", "public.x").with_tenant("acme");
        let r_globex = ResourceRef::new("table", "public.x").with_tenant("globex");
        let mut ctx = EvalContext::default();
        ctx.current_tenant = Some("acme".into());
        assert!(matches!(
            evaluate(&[&p], "select", &r_acme, &ctx),
            Decision::Allow { .. }
        ));
        assert_eq!(
            evaluate(&[&p], "select", &r_globex, &ctx),
            Decision::DefaultDeny
        );
    }

    // -----------------------------------------------------------------
    // Simulator
    // -----------------------------------------------------------------

    #[test]
    fn simulator_produces_trail() {
        let allow = analyst_policy();
        let deny = no_deletes_policy();
        let r = ResourceRef::new("table", "public.orders");
        let out = simulate(&[&allow, &deny], "delete", &r, &EvalContext::default());
        // Two policies, each with one statement → at least one trail
        // entry per statement.
        assert!(out.trail.len() >= 2);
        assert!(matches!(out.decision, Decision::Deny { .. }));
        assert!(out.reason.contains("deny"));
    }

    // -----------------------------------------------------------------
    // Misc helpers
    // -----------------------------------------------------------------

    #[test]
    fn rfc3339_parses_to_ms() {
        let ms = parse_rfc3339_ms("1970-01-01T00:00:00Z").unwrap();
        assert_eq!(ms, 0);
        let ms = parse_rfc3339_ms("1970-01-01T00:00:01.500Z").unwrap();
        assert_eq!(ms, 1_500);
        let ms = parse_rfc3339_ms("2024-01-01T00:00:00+00:00").unwrap();
        // 2024-01-01 = 19723 days after epoch.
        assert_eq!(ms, 19_723u128 * 86_400_000);
    }

    #[test]
    fn rfc3339_handles_negative_offset() {
        // 2024-01-01T01:00:00+01:00 == 2024-01-01T00:00:00Z
        let a = parse_rfc3339_ms("2024-01-01T01:00:00+01:00").unwrap();
        let b = parse_rfc3339_ms("2024-01-01T00:00:00Z").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn cidr_v6_basic() {
        let c = parse_cidr("::1/128").unwrap();
        assert_eq!(c.prefix_len, 128);
        assert!(cidr_contains(&c, IpAddr::from_str("::1").unwrap()));
        assert!(!cidr_contains(&c, IpAddr::from_str("::2").unwrap()));
    }
}
