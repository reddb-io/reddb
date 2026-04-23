//! RESTful, collection-centric Git-for-Data HTTP surface.
//!
//! Design principles:
//! - Nouns, not verbs: `/repo/commits`, `/repo/refs/heads/<name>`.
//! - HTTP semantics: GET for reads, POST creates, PUT moves refs,
//!   DELETE deletes, nested resources for merge conflicts.
//! - Session-scoped state transitions on `/repo/sessions/<conn>/*`
//!   instead of opaque `connection_id` fields sprinkled in bodies.
//! - Developer-centric collections: `/collections/<name>/vcs`
//!   owns the opt-in toggle because that's how a dev thinks about
//!   it — the collection is the resource, VCS is an aspect of it.

use super::transport::{json_response, HttpResponse};
use crate::application::vcs_payload::{
    commit_hash_to_json, commit_to_json, commits_to_json, conflicts_to_json, diff_to_json,
    merge_outcome_to_json, ref_to_json, refs_to_json, status_to_json,
};
use crate::application::{
    AsOfSpec, Author, CheckoutInput, CheckoutTarget, CreateBranchInput, CreateCommitInput,
    CreateTagInput, DiffInput, LogInput, LogRange, MergeInput, MergeOpts, MergeStrategy, ResetInput,
    ResetMode, StatusInput, VcsUseCases,
};
use crate::json::{from_slice as json_from_slice, Map, Value as JsonValue};
use crate::runtime::RedDBRuntime;
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Envelope helpers
// ---------------------------------------------------------------------------

fn err(msg: &str) -> JsonValue {
    let mut map = Map::new();
    map.insert("ok".to_string(), JsonValue::Bool(false));
    map.insert("error".to_string(), JsonValue::String(msg.to_string()));
    JsonValue::Object(map)
}

fn ok(value: JsonValue) -> JsonValue {
    let mut map = Map::new();
    map.insert("ok".to_string(), JsonValue::Bool(true));
    map.insert("result".to_string(), value);
    JsonValue::Object(map)
}

fn parse_body(body: Vec<u8>) -> Result<JsonValue, HttpResponse> {
    if body.is_empty() {
        return Ok(JsonValue::Object(Map::new()));
    }
    json_from_slice::<JsonValue>(&body)
        .map_err(|e| json_response(400, err(&format!("invalid json: {e}"))))
}

fn vcs(runtime: &RedDBRuntime) -> VcsUseCases<'_, RedDBRuntime> {
    VcsUseCases::new(runtime)
}

fn required_str<'a>(body: &'a JsonValue, key: &str) -> Result<&'a str, HttpResponse> {
    body.get(key)
        .and_then(JsonValue::as_str)
        .ok_or_else(|| json_response(400, err(&format!("missing required field `{key}`"))))
}

fn parse_author(body: &JsonValue) -> Result<Author, HttpResponse> {
    let obj = body
        .get("author")
        .ok_or_else(|| json_response(400, err("missing required field `author`")))?;
    let name = obj
        .get("name")
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .to_string();
    let email = obj
        .get("email")
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .to_string();
    Ok(Author { name, email })
}

fn parse_u64(s: &str) -> Option<u64> {
    s.parse().ok()
}

fn error_status_for(msg: &str) -> u16 {
    // Best-effort mapping — RedDBError display strings start with
    // "not found:", "invalid config:", "read-only violation:".
    if msg.contains("not found") {
        404
    } else if msg.contains("read-only") || msg.contains("protected") {
        409
    } else if msg.contains("invalid config") {
        400
    } else {
        500
    }
}

fn map_err_response(e: impl std::fmt::Display) -> HttpResponse {
    let s = e.to_string();
    json_response(error_status_for(&s), err(&s))
}

// ---------------------------------------------------------------------------
// /repo — summary
// ---------------------------------------------------------------------------

pub(crate) fn handle_repo_info(runtime: &RedDBRuntime) -> HttpResponse {
    let vcs = vcs(runtime);
    let mut summary = Map::new();

    let branches = vcs.branch_list().unwrap_or_default();
    let tags = vcs.tag_list().unwrap_or_default();
    let versioned = vcs.list_versioned().unwrap_or_default();

    summary.insert(
        "branches".to_string(),
        JsonValue::Number(branches.len() as f64),
    );
    summary.insert("tags".to_string(), JsonValue::Number(tags.len() as f64));
    summary.insert(
        "versioned_collections".to_string(),
        JsonValue::Array(versioned.into_iter().map(JsonValue::String).collect()),
    );
    summary.insert(
        "default_branch".to_string(),
        JsonValue::String("refs/heads/main".to_string()),
    );
    json_response(200, ok(JsonValue::Object(summary)))
}

// ---------------------------------------------------------------------------
// /repo/refs — unified ref listing
// ---------------------------------------------------------------------------

pub(crate) fn handle_refs_list(
    runtime: &RedDBRuntime,
    query: &BTreeMap<String, String>,
) -> HttpResponse {
    let prefix = query.get("prefix").map(String::as_str);
    match runtime.vcs_list_refs(prefix) {
        Ok(refs) => json_response(200, ok(refs_to_json(&refs))),
        Err(e) => map_err_response(e),
    }
}

// ---------------------------------------------------------------------------
// /repo/refs/heads — branches
// ---------------------------------------------------------------------------

pub(crate) fn handle_branches_list(runtime: &RedDBRuntime) -> HttpResponse {
    match vcs(runtime).branch_list() {
        Ok(refs) => json_response(200, ok(refs_to_json(&refs))),
        Err(e) => map_err_response(e),
    }
}

pub(crate) fn handle_branch_create(runtime: &RedDBRuntime, body: Vec<u8>) -> HttpResponse {
    let body = match parse_body(body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let name = match required_str(&body, "name") {
        Ok(s) => s.to_string(),
        Err(resp) => return resp,
    };
    let connection_id = body
        .get("connection_id")
        .and_then(JsonValue::as_u64)
        .unwrap_or(0);
    let from = body
        .get("from")
        .and_then(JsonValue::as_str)
        .map(String::from);
    match vcs(runtime).branch_create(CreateBranchInput {
        name,
        from,
        connection_id,
    }) {
        Ok(r) => json_response(201, ok(ref_to_json(&r))),
        Err(e) => map_err_response(e),
    }
}

pub(crate) fn handle_branch_show(runtime: &RedDBRuntime, name: &str) -> HttpResponse {
    let full = if name.starts_with("refs/heads/") {
        name.to_string()
    } else {
        format!("refs/heads/{name}")
    };
    match runtime.vcs_list_refs(Some(&full)) {
        Ok(refs) => {
            match refs.into_iter().find(|r| r.name == full) {
                Some(r) => json_response(200, ok(ref_to_json(&r))),
                None => json_response(404, err(&format!("branch `{name}` not found"))),
            }
        }
        Err(e) => map_err_response(e),
    }
}

pub(crate) fn handle_branch_move(
    runtime: &RedDBRuntime,
    name: &str,
    body: Vec<u8>,
) -> HttpResponse {
    // PUT /repo/refs/heads/{name} { commit: <hash> } — move the
    // branch ref to a new commit. Implemented via reset --soft so
    // workset tracking stays in sync.
    let body = match parse_body(body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let target = match required_str(&body, "commit") {
        Ok(s) => s.to_string(),
        Err(resp) => return resp,
    };
    let connection_id = body
        .get("connection_id")
        .and_then(JsonValue::as_u64)
        .unwrap_or(0);
    match vcs(runtime).reset(ResetInput {
        connection_id,
        target,
        mode: ResetMode::Soft,
    }) {
        Ok(()) => handle_branch_show(runtime, name),
        Err(e) => map_err_response(e),
    }
}

pub(crate) fn handle_branch_delete(runtime: &RedDBRuntime, name: &str) -> HttpResponse {
    match vcs(runtime).branch_delete(name) {
        Ok(()) => json_response(204, JsonValue::Null),
        Err(e) => map_err_response(e),
    }
}

// ---------------------------------------------------------------------------
// /repo/refs/tags
// ---------------------------------------------------------------------------

pub(crate) fn handle_tags_list(runtime: &RedDBRuntime) -> HttpResponse {
    match vcs(runtime).tag_list() {
        Ok(refs) => json_response(200, ok(refs_to_json(&refs))),
        Err(e) => map_err_response(e),
    }
}

pub(crate) fn handle_tag_create(runtime: &RedDBRuntime, body: Vec<u8>) -> HttpResponse {
    let body = match parse_body(body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let name = match required_str(&body, "name") {
        Ok(s) => s.to_string(),
        Err(resp) => return resp,
    };
    let target = match required_str(&body, "target") {
        Ok(s) => s.to_string(),
        Err(resp) => return resp,
    };
    let annotation = body
        .get("annotation")
        .and_then(JsonValue::as_str)
        .map(String::from);
    match vcs(runtime).tag(CreateTagInput {
        name,
        target,
        annotation,
    }) {
        Ok(r) => json_response(201, ok(ref_to_json(&r))),
        Err(e) => map_err_response(e),
    }
}

pub(crate) fn handle_tag_show(runtime: &RedDBRuntime, name: &str) -> HttpResponse {
    let full = if name.starts_with("refs/tags/") {
        name.to_string()
    } else {
        format!("refs/tags/{name}")
    };
    match runtime.vcs_list_refs(Some(&full)) {
        Ok(refs) => match refs.into_iter().find(|r| r.name == full) {
            Some(r) => json_response(200, ok(ref_to_json(&r))),
            None => json_response(404, err(&format!("tag `{name}` not found"))),
        },
        Err(e) => map_err_response(e),
    }
}

pub(crate) fn handle_tag_delete(runtime: &RedDBRuntime, name: &str) -> HttpResponse {
    let full = if name.starts_with("refs/tags/") {
        name.to_string()
    } else {
        format!("refs/tags/{name}")
    };
    match runtime.vcs_list_refs(Some(&full)) {
        Ok(refs) if refs.iter().any(|r| r.name == full) => {
            match runtime.vcs_branch_delete(&full) {
                Ok(()) => json_response(204, JsonValue::Null),
                Err(e) => map_err_response(e),
            }
        }
        Ok(_) => json_response(404, err(&format!("tag `{name}` not found"))),
        Err(e) => map_err_response(e),
    }
}

// ---------------------------------------------------------------------------
// /repo/commits — log, show, create
// ---------------------------------------------------------------------------

pub(crate) fn handle_commits_list(
    runtime: &RedDBRuntime,
    query: &BTreeMap<String, String>,
) -> HttpResponse {
    let range = LogRange {
        to: query
            .get("to")
            .or_else(|| query.get("branch"))
            .map(String::from),
        from: query.get("from").map(String::from),
        limit: query.get("limit").and_then(|s| s.parse().ok()),
        skip: query.get("skip").and_then(|s| s.parse().ok()),
        no_merges: query
            .get("no_merges")
            .map(|s| s == "true" || s == "1")
            .unwrap_or(false),
    };
    let connection_id = query
        .get("connection_id")
        .and_then(|s| parse_u64(s))
        .unwrap_or(0);
    match vcs(runtime).log(LogInput {
        connection_id,
        range,
    }) {
        Ok(commits) => json_response(200, ok(commits_to_json(&commits))),
        Err(e) => map_err_response(e),
    }
}

pub(crate) fn handle_commit_show(runtime: &RedDBRuntime, hash: &str) -> HttpResponse {
    match vcs(runtime).log(LogInput {
        connection_id: 0,
        range: LogRange {
            to: Some(hash.to_string()),
            from: None,
            limit: Some(1),
            skip: None,
            no_merges: false,
        },
    }) {
        Ok(commits) if !commits.is_empty() => {
            json_response(200, ok(commit_to_json(&commits[0])))
        }
        Ok(_) => json_response(404, err(&format!("commit `{hash}` not found"))),
        Err(e) => map_err_response(e),
    }
}

pub(crate) fn handle_commit_create(runtime: &RedDBRuntime, body: Vec<u8>) -> HttpResponse {
    let body = match parse_body(body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let connection_id = body
        .get("connection_id")
        .and_then(JsonValue::as_u64)
        .unwrap_or(0);
    let message = match required_str(&body, "message") {
        Ok(s) => s.to_string(),
        Err(resp) => return resp,
    };
    let author = match parse_author(&body) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let committer = body.get("committer").and_then(|obj| {
        let name = obj.get("name").and_then(JsonValue::as_str).unwrap_or("");
        let email = obj.get("email").and_then(JsonValue::as_str).unwrap_or("");
        if name.is_empty() && email.is_empty() {
            None
        } else {
            Some(Author {
                name: name.to_string(),
                email: email.to_string(),
            })
        }
    });
    let amend = body
        .get("amend")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    let allow_empty = body
        .get("allow_empty")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    match vcs(runtime).commit(CreateCommitInput {
        connection_id,
        message,
        author,
        committer,
        amend,
        allow_empty,
    }) {
        Ok(c) => json_response(201, ok(commit_to_json(&c))),
        Err(e) => map_err_response(e),
    }
}

pub(crate) fn handle_commit_diff(
    runtime: &RedDBRuntime,
    from: &str,
    to: &str,
    query: &BTreeMap<String, String>,
) -> HttpResponse {
    let collection = query.get("collection").map(String::from);
    let summary_only = query
        .get("summary")
        .map(|s| s == "true" || s == "1")
        .unwrap_or(false);
    match vcs(runtime).diff(DiffInput {
        from: from.to_string(),
        to: to.to_string(),
        collection,
        summary_only,
    }) {
        Ok(d) => json_response(200, ok(diff_to_json(&d))),
        Err(e) => map_err_response(e),
    }
}

pub(crate) fn handle_commit_lca(
    runtime: &RedDBRuntime,
    a: &str,
    b: &str,
) -> HttpResponse {
    match vcs(runtime).lca(a, b) {
        Ok(hash) => {
            let mut map = Map::new();
            map.insert("lca".to_string(), commit_hash_to_json(hash.as_ref()));
            json_response(200, ok(JsonValue::Object(map)))
        }
        Err(e) => map_err_response(e),
    }
}

// ---------------------------------------------------------------------------
// /repo/sessions/{conn} — per-connection workset + state transitions
// ---------------------------------------------------------------------------

pub(crate) fn handle_session_status(runtime: &RedDBRuntime, conn: u64) -> HttpResponse {
    match vcs(runtime).status(StatusInput {
        connection_id: conn,
    }) {
        Ok(s) => json_response(200, ok(status_to_json(&s))),
        Err(e) => map_err_response(e),
    }
}

pub(crate) fn handle_session_checkout(
    runtime: &RedDBRuntime,
    conn: u64,
    body: Vec<u8>,
) -> HttpResponse {
    let body = match parse_body(body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let kind = body
        .get("kind")
        .and_then(JsonValue::as_str)
        .unwrap_or("branch");
    let target_val = match required_str(&body, "target") {
        Ok(s) => s.to_string(),
        Err(resp) => return resp,
    };
    let target = match kind {
        "branch" => CheckoutTarget::Branch(target_val),
        "commit" => CheckoutTarget::Commit(target_val),
        "tag" => CheckoutTarget::Tag(target_val),
        other => {
            return json_response(
                400,
                err(&format!("unknown kind `{other}` (expected branch|commit|tag)")),
            );
        }
    };
    let force = body
        .get("force")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    match vcs(runtime).checkout(CheckoutInput {
        connection_id: conn,
        target,
        force,
    }) {
        Ok(r) => json_response(200, ok(ref_to_json(&r))),
        Err(e) => map_err_response(e),
    }
}

pub(crate) fn handle_session_merge(
    runtime: &RedDBRuntime,
    conn: u64,
    body: Vec<u8>,
) -> HttpResponse {
    let body = match parse_body(body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let from = match required_str(&body, "from") {
        Ok(s) => s.to_string(),
        Err(resp) => return resp,
    };
    let author = match parse_author(&body) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let strategy = match body.get("strategy").and_then(JsonValue::as_str) {
        Some("ff-only") | Some("fast_forward_only") => MergeStrategy::FastForwardOnly,
        Some("no-ff") | Some("no_fast_forward") => MergeStrategy::NoFastForward,
        _ => MergeStrategy::Auto,
    };
    let message = body
        .get("message")
        .and_then(JsonValue::as_str)
        .map(String::from);
    let abort_on_conflict = body
        .get("abort_on_conflict")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    match vcs(runtime).merge(MergeInput {
        connection_id: conn,
        from,
        opts: MergeOpts {
            strategy,
            message,
            abort_on_conflict,
        },
        author,
    }) {
        Ok(outcome) => json_response(200, ok(merge_outcome_to_json(&outcome))),
        Err(e) => map_err_response(e),
    }
}

pub(crate) fn handle_session_reset(
    runtime: &RedDBRuntime,
    conn: u64,
    body: Vec<u8>,
) -> HttpResponse {
    let body = match parse_body(body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let target = match required_str(&body, "target") {
        Ok(s) => s.to_string(),
        Err(resp) => return resp,
    };
    let mode = match body.get("mode").and_then(JsonValue::as_str) {
        Some("soft") => ResetMode::Soft,
        Some("hard") => ResetMode::Hard,
        _ => ResetMode::Mixed,
    };
    match vcs(runtime).reset(ResetInput {
        connection_id: conn,
        target,
        mode,
    }) {
        Ok(()) => handle_session_status(runtime, conn),
        Err(e) => map_err_response(e),
    }
}

pub(crate) fn handle_session_cherry_pick(
    runtime: &RedDBRuntime,
    conn: u64,
    body: Vec<u8>,
) -> HttpResponse {
    let body = match parse_body(body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let commit = match required_str(&body, "commit") {
        Ok(s) => s.to_string(),
        Err(resp) => return resp,
    };
    let author = match parse_author(&body) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    match vcs(runtime).cherry_pick(conn, &commit, author) {
        Ok(outcome) => json_response(200, ok(merge_outcome_to_json(&outcome))),
        Err(e) => map_err_response(e),
    }
}

pub(crate) fn handle_session_revert(
    runtime: &RedDBRuntime,
    conn: u64,
    body: Vec<u8>,
) -> HttpResponse {
    let body = match parse_body(body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let commit = match required_str(&body, "commit") {
        Ok(s) => s.to_string(),
        Err(resp) => return resp,
    };
    let author = match parse_author(&body) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    match vcs(runtime).revert(conn, &commit, author) {
        Ok(c) => json_response(200, ok(commit_to_json(&c))),
        Err(e) => map_err_response(e),
    }
}

// ---------------------------------------------------------------------------
// /repo/merges/{msid} — merge state + nested conflicts
// ---------------------------------------------------------------------------

pub(crate) fn handle_merge_show(_runtime: &RedDBRuntime, msid: &str) -> HttpResponse {
    // For now merge state is surfaced only via the conflict list —
    // a full "get merge state row" method can be added in a follow-up
    // as the runtime exposes it.
    let mut map = Map::new();
    map.insert("id".to_string(), JsonValue::String(msid.to_string()));
    map.insert(
        "conflicts_url".to_string(),
        JsonValue::String(format!("/repo/merges/{msid}/conflicts")),
    );
    json_response(200, ok(JsonValue::Object(map)))
}

pub(crate) fn handle_merge_conflicts(runtime: &RedDBRuntime, msid: &str) -> HttpResponse {
    match vcs(runtime).conflicts_list(msid) {
        Ok(list) => json_response(200, ok(conflicts_to_json(&list))),
        Err(e) => map_err_response(e),
    }
}

pub(crate) fn handle_conflict_resolve(
    runtime: &RedDBRuntime,
    _msid: &str,
    cid: &str,
    body: Vec<u8>,
) -> HttpResponse {
    let body = match parse_body(body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let value = body.get("value").cloned().unwrap_or(JsonValue::Null);
    match vcs(runtime).conflict_resolve(cid, value) {
        Ok(()) => json_response(204, JsonValue::Null),
        Err(e) => map_err_response(e),
    }
}

// ---------------------------------------------------------------------------
// /collections/{name}/vcs — collection-centric opt-in toggle
// ---------------------------------------------------------------------------

pub(crate) fn handle_collection_vcs_show(
    runtime: &RedDBRuntime,
    name: &str,
) -> HttpResponse {
    match vcs(runtime).is_versioned(name) {
        Ok(versioned) => {
            let mut map = Map::new();
            map.insert("collection".to_string(), JsonValue::String(name.to_string()));
            map.insert("versioned".to_string(), JsonValue::Bool(versioned));
            json_response(200, ok(JsonValue::Object(map)))
        }
        Err(e) => map_err_response(e),
    }
}

pub(crate) fn handle_collection_vcs_set(
    runtime: &RedDBRuntime,
    name: &str,
    body: Vec<u8>,
) -> HttpResponse {
    let body = match parse_body(body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let enabled = body
        .get("versioned")
        .or_else(|| body.get("enabled"))
        .and_then(JsonValue::as_bool)
        .ok_or_else(|| json_response(400, err("missing boolean field `versioned`")));
    let enabled = match enabled {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    match vcs(runtime).set_versioned(name, enabled) {
        Ok(()) => handle_collection_vcs_show(runtime, name),
        Err(e) => map_err_response(e),
    }
}

// ---------------------------------------------------------------------------
// Consume unused AsOfSpec import (kept for future /collections/{name}/at/...)
// ---------------------------------------------------------------------------

#[allow(dead_code)]
fn _touch(_: AsOfSpec) {}
