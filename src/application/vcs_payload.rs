//! JSON payload parsers for VCS endpoints.
//!
//! Transport layers (gRPC, REST, CLI) pass a generic `payload: Value`
//! into the application layer. This module turns that payload into the
//! typed Input structs declared in [`crate::application::vcs`]. Output
//! formatting (Commit → JSON, Diff → JSON, etc.) also lives here so the
//! presentation layer stays thin.

use crate::application::json_input::{
    json_bool_field, json_string_field, json_string_list_field, json_usize_field,
};
use crate::application::vcs::{
    AsOfSpec, Author, CheckoutInput, CheckoutTarget, Commit, CommitHash, Conflict,
    CreateBranchInput, CreateCommitInput, CreateTagInput, Diff, DiffChange, DiffEntry, DiffInput,
    LogInput, LogRange, MergeInput, MergeOpts, MergeOutcome, MergeStrategy, Ref, RefKind,
    ResetInput, ResetMode, Status, StatusInput,
};
use crate::json::{Map, Value as JsonValue};
use crate::{RedDBError, RedDBResult};

// ---------------------------------------------------------------------------
// Common helpers
// ---------------------------------------------------------------------------

fn required_string(payload: &JsonValue, key: &str) -> RedDBResult<String> {
    json_string_field(payload, key)
        .ok_or_else(|| RedDBError::InvalidConfig(format!("missing required field `{key}`")))
}

fn parse_author(payload: &JsonValue, key: &str) -> RedDBResult<Author> {
    let obj = payload
        .get(key)
        .ok_or_else(|| RedDBError::InvalidConfig(format!("missing required field `{key}`")))?;
    Ok(Author {
        name: json_string_field(obj, "name").unwrap_or_default(),
        email: json_string_field(obj, "email").unwrap_or_default(),
    })
}

fn parse_optional_author(payload: &JsonValue, key: &str) -> Option<Author> {
    let obj = payload.get(key)?;
    Some(Author {
        name: json_string_field(obj, "name").unwrap_or_default(),
        email: json_string_field(obj, "email").unwrap_or_default(),
    })
}

fn connection_id_field(payload: &JsonValue) -> RedDBResult<u64> {
    payload
        .get("connection_id")
        .and_then(JsonValue::as_u64)
        .ok_or_else(|| {
            RedDBError::InvalidConfig("missing required field `connection_id`".to_string())
        })
}

// ---------------------------------------------------------------------------
// Input parsers
// ---------------------------------------------------------------------------

pub fn parse_create_commit_input(payload: &JsonValue) -> RedDBResult<CreateCommitInput> {
    Ok(CreateCommitInput {
        connection_id: connection_id_field(payload)?,
        message: required_string(payload, "message")?,
        author: parse_author(payload, "author")?,
        committer: parse_optional_author(payload, "committer"),
        amend: json_bool_field(payload, "amend").unwrap_or(false),
        allow_empty: json_bool_field(payload, "allow_empty").unwrap_or(false),
    })
}

pub fn parse_create_branch_input(payload: &JsonValue) -> RedDBResult<CreateBranchInput> {
    Ok(CreateBranchInput {
        name: required_string(payload, "name")?,
        from: json_string_field(payload, "from"),
        connection_id: connection_id_field(payload)?,
    })
}

pub fn parse_create_tag_input(payload: &JsonValue) -> RedDBResult<CreateTagInput> {
    Ok(CreateTagInput {
        name: required_string(payload, "name")?,
        target: required_string(payload, "target")?,
        annotation: json_string_field(payload, "annotation"),
    })
}

pub fn parse_checkout_input(payload: &JsonValue) -> RedDBResult<CheckoutInput> {
    let kind = json_string_field(payload, "kind").unwrap_or_else(|| "branch".to_string());
    let value = required_string(payload, "target")?;
    let target = match kind.as_str() {
        "branch" => CheckoutTarget::Branch(value),
        "commit" => CheckoutTarget::Commit(value),
        "tag" => CheckoutTarget::Tag(value),
        other => {
            return Err(RedDBError::InvalidConfig(format!(
                "unknown checkout kind `{other}`; expected branch|commit|tag"
            )));
        }
    };
    Ok(CheckoutInput {
        connection_id: connection_id_field(payload)?,
        target,
        force: json_bool_field(payload, "force").unwrap_or(false),
    })
}

pub fn parse_merge_strategy(value: Option<&str>) -> MergeStrategy {
    match value.unwrap_or("auto") {
        "no-ff" | "no_fast_forward" => MergeStrategy::NoFastForward,
        "ff-only" | "fast_forward_only" => MergeStrategy::FastForwardOnly,
        _ => MergeStrategy::Auto,
    }
}

pub fn parse_merge_input(payload: &JsonValue) -> RedDBResult<MergeInput> {
    let opts = MergeOpts {
        strategy: parse_merge_strategy(payload.get("strategy").and_then(JsonValue::as_str)),
        message: json_string_field(payload, "message"),
        abort_on_conflict: json_bool_field(payload, "abort_on_conflict").unwrap_or(false),
    };
    Ok(MergeInput {
        connection_id: connection_id_field(payload)?,
        from: required_string(payload, "from")?,
        opts,
        author: parse_author(payload, "author")?,
    })
}

pub fn parse_reset_mode(value: Option<&str>) -> ResetMode {
    match value.unwrap_or("mixed") {
        "soft" => ResetMode::Soft,
        "hard" => ResetMode::Hard,
        _ => ResetMode::Mixed,
    }
}

pub fn parse_reset_input(payload: &JsonValue) -> RedDBResult<ResetInput> {
    Ok(ResetInput {
        connection_id: connection_id_field(payload)?,
        target: required_string(payload, "target")?,
        mode: parse_reset_mode(payload.get("mode").and_then(JsonValue::as_str)),
    })
}

pub fn parse_log_input(payload: &JsonValue) -> RedDBResult<LogInput> {
    let range = LogRange {
        to: json_string_field(payload, "to"),
        from: json_string_field(payload, "from"),
        limit: json_usize_field(payload, "limit"),
        skip: json_usize_field(payload, "skip"),
        no_merges: json_bool_field(payload, "no_merges").unwrap_or(false),
    };
    let _ = json_string_list_field(payload, "tables"); // reserved for future path filter
    Ok(LogInput {
        connection_id: connection_id_field(payload).unwrap_or(0),
        range,
    })
}

pub fn parse_diff_input(payload: &JsonValue) -> RedDBResult<DiffInput> {
    Ok(DiffInput {
        from: required_string(payload, "from")?,
        to: required_string(payload, "to")?,
        collection: json_string_field(payload, "collection"),
        summary_only: json_bool_field(payload, "summary_only").unwrap_or(false),
    })
}

pub fn parse_status_input(payload: &JsonValue) -> RedDBResult<StatusInput> {
    Ok(StatusInput {
        connection_id: connection_id_field(payload)?,
    })
}

pub fn parse_as_of_spec(payload: &JsonValue) -> RedDBResult<AsOfSpec> {
    let kind = json_string_field(payload, "kind").unwrap_or_else(|| "commit".to_string());
    match kind.as_str() {
        "commit" => Ok(AsOfSpec::Commit(required_string(payload, "value")?)),
        "branch" => Ok(AsOfSpec::Branch(required_string(payload, "value")?)),
        "tag" => Ok(AsOfSpec::Tag(required_string(payload, "value")?)),
        "timestamp" => {
            let ms = payload
                .get("value")
                .and_then(JsonValue::as_i64)
                .ok_or_else(|| {
                    RedDBError::InvalidConfig(
                        "as_of timestamp requires numeric `value` in ms".to_string(),
                    )
                })?;
            Ok(AsOfSpec::TimestampMs(ms))
        }
        "snapshot" => {
            let x = payload
                .get("value")
                .and_then(JsonValue::as_u64)
                .ok_or_else(|| {
                    RedDBError::InvalidConfig("as_of snapshot requires numeric `value`".to_string())
                })?;
            Ok(AsOfSpec::Snapshot(x))
        }
        other => Err(RedDBError::InvalidConfig(format!(
            "unknown as_of kind `{other}`"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Output encoders
// ---------------------------------------------------------------------------

pub fn author_to_json(a: &Author) -> JsonValue {
    let mut map = Map::new();
    map.insert("name".to_string(), JsonValue::String(a.name.clone()));
    map.insert("email".to_string(), JsonValue::String(a.email.clone()));
    JsonValue::Object(map)
}

pub fn commit_to_json(c: &Commit) -> JsonValue {
    let mut map = Map::new();
    map.insert("hash".to_string(), JsonValue::String(c.hash.clone()));
    map.insert("root_xid".to_string(), JsonValue::Number(c.root_xid as f64));
    map.insert(
        "parents".to_string(),
        JsonValue::Array(
            c.parents
                .iter()
                .map(|h| JsonValue::String(h.clone()))
                .collect(),
        ),
    );
    map.insert("height".to_string(), JsonValue::Number(c.height as f64));
    map.insert("author".to_string(), author_to_json(&c.author));
    map.insert("committer".to_string(), author_to_json(&c.committer));
    map.insert("message".to_string(), JsonValue::String(c.message.clone()));
    map.insert(
        "timestamp_ms".to_string(),
        JsonValue::Number(c.timestamp_ms as f64),
    );
    if let Some(sig) = &c.signature {
        map.insert("signature".to_string(), JsonValue::String(sig.clone()));
    }
    JsonValue::Object(map)
}

pub fn ref_kind_str(kind: RefKind) -> &'static str {
    match kind {
        RefKind::Branch => "branch",
        RefKind::Tag => "tag",
        RefKind::Head => "head",
    }
}

pub fn ref_to_json(r: &Ref) -> JsonValue {
    let mut map = Map::new();
    map.insert("name".to_string(), JsonValue::String(r.name.clone()));
    map.insert(
        "kind".to_string(),
        JsonValue::String(ref_kind_str(r.kind).to_string()),
    );
    map.insert("target".to_string(), JsonValue::String(r.target.clone()));
    map.insert("protected".to_string(), JsonValue::Bool(r.protected));
    JsonValue::Object(map)
}

pub fn diff_entry_to_json(entry: &DiffEntry) -> JsonValue {
    let mut map = Map::new();
    map.insert(
        "collection".to_string(),
        JsonValue::String(entry.collection.clone()),
    );
    map.insert(
        "entity_id".to_string(),
        JsonValue::String(entry.entity_id.clone()),
    );
    match &entry.change {
        DiffChange::Added { after } => {
            map.insert("change".to_string(), JsonValue::String("added".to_string()));
            map.insert("after".to_string(), after.clone());
        }
        DiffChange::Removed { before } => {
            map.insert(
                "change".to_string(),
                JsonValue::String("removed".to_string()),
            );
            map.insert("before".to_string(), before.clone());
        }
        DiffChange::Modified { before, after } => {
            map.insert(
                "change".to_string(),
                JsonValue::String("modified".to_string()),
            );
            map.insert("before".to_string(), before.clone());
            map.insert("after".to_string(), after.clone());
        }
    }
    JsonValue::Object(map)
}

pub fn diff_to_json(diff: &Diff) -> JsonValue {
    let mut map = Map::new();
    map.insert("from".to_string(), JsonValue::String(diff.from.clone()));
    map.insert("to".to_string(), JsonValue::String(diff.to.clone()));
    map.insert("added".to_string(), JsonValue::Number(diff.added as f64));
    map.insert(
        "removed".to_string(),
        JsonValue::Number(diff.removed as f64),
    );
    map.insert(
        "modified".to_string(),
        JsonValue::Number(diff.modified as f64),
    );
    map.insert(
        "entries".to_string(),
        JsonValue::Array(diff.entries.iter().map(diff_entry_to_json).collect()),
    );
    JsonValue::Object(map)
}

pub fn conflict_to_json(c: &Conflict) -> JsonValue {
    let mut map = Map::new();
    map.insert("id".to_string(), JsonValue::String(c.id.clone()));
    map.insert(
        "collection".to_string(),
        JsonValue::String(c.collection.clone()),
    );
    map.insert(
        "entity_id".to_string(),
        JsonValue::String(c.entity_id.clone()),
    );
    map.insert("base".to_string(), c.base.clone());
    map.insert("ours".to_string(), c.ours.clone());
    map.insert("theirs".to_string(), c.theirs.clone());
    map.insert(
        "conflicting_paths".to_string(),
        JsonValue::Array(
            c.conflicting_paths
                .iter()
                .map(|p| JsonValue::String(p.clone()))
                .collect(),
        ),
    );
    map.insert(
        "merge_state_id".to_string(),
        JsonValue::String(c.merge_state_id.clone()),
    );
    JsonValue::Object(map)
}

pub fn merge_outcome_to_json(outcome: &MergeOutcome) -> JsonValue {
    let mut map = Map::new();
    map.insert(
        "fast_forward".to_string(),
        JsonValue::Bool(outcome.fast_forward),
    );
    map.insert(
        "conflicts".to_string(),
        JsonValue::Array(outcome.conflicts.iter().map(conflict_to_json).collect()),
    );
    if let Some(c) = &outcome.merge_commit {
        map.insert("merge_commit".to_string(), commit_to_json(c));
    }
    if let Some(id) = &outcome.merge_state_id {
        map.insert("merge_state_id".to_string(), JsonValue::String(id.clone()));
    }
    JsonValue::Object(map)
}

pub fn status_to_json(s: &Status) -> JsonValue {
    let mut map = Map::new();
    map.insert(
        "connection_id".to_string(),
        JsonValue::Number(s.connection_id as f64),
    );
    if let Some(r) = &s.head_ref {
        map.insert("head_ref".to_string(), JsonValue::String(r.clone()));
    }
    if let Some(c) = &s.head_commit {
        map.insert("head_commit".to_string(), JsonValue::String(c.clone()));
    }
    map.insert("detached".to_string(), JsonValue::Bool(s.detached));
    map.insert(
        "staged_changes".to_string(),
        JsonValue::Number(s.staged_changes as f64),
    );
    map.insert(
        "working_changes".to_string(),
        JsonValue::Number(s.working_changes as f64),
    );
    map.insert(
        "unresolved_conflicts".to_string(),
        JsonValue::Number(s.unresolved_conflicts as f64),
    );
    if let Some(id) = &s.merge_state_id {
        map.insert("merge_state_id".to_string(), JsonValue::String(id.clone()));
    }
    JsonValue::Object(map)
}

pub fn commits_to_json(list: &[Commit]) -> JsonValue {
    JsonValue::Array(list.iter().map(commit_to_json).collect())
}

pub fn refs_to_json(list: &[Ref]) -> JsonValue {
    JsonValue::Array(list.iter().map(ref_to_json).collect())
}

pub fn conflicts_to_json(list: &[Conflict]) -> JsonValue {
    JsonValue::Array(list.iter().map(conflict_to_json).collect())
}

pub fn commit_hash_to_json(hash: Option<&CommitHash>) -> JsonValue {
    match hash {
        Some(h) => JsonValue::String(h.clone()),
        None => JsonValue::Null,
    }
}
