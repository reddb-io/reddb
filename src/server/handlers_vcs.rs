//! HTTP handlers for the VCS ("Git for Data") surface.
//!
//! Mirrors the pattern in `handlers_graph.rs`: parse JSON body into a
//! typed input, invoke `VcsUseCases`, encode output via
//! `application::vcs_payload` helpers.

use super::transport::{json_response, HttpResponse};
use crate::application::vcs_payload::{
    commit_hash_to_json, commit_to_json, commits_to_json, conflicts_to_json, diff_to_json,
    merge_outcome_to_json, parse_checkout_input, parse_create_branch_input,
    parse_create_commit_input, parse_create_tag_input, parse_diff_input, parse_log_input,
    parse_merge_input, parse_reset_input, parse_status_input, ref_to_json, refs_to_json,
    status_to_json,
};
use crate::application::VcsUseCases;
use crate::json::{from_slice as json_from_slice, Map, Value as JsonValue};
use crate::runtime::RedDBRuntime;

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

fn use_cases(runtime: &RedDBRuntime) -> VcsUseCases<'_, RedDBRuntime> {
    VcsUseCases::new(runtime)
}

pub(crate) fn handle_commit(runtime: &RedDBRuntime, body: Vec<u8>) -> HttpResponse {
    let body = match parse_body(body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let input = match parse_create_commit_input(&body) {
        Ok(v) => v,
        Err(e) => return json_response(400, err(&e.to_string())),
    };
    match use_cases(runtime).commit(input) {
        Ok(c) => json_response(200, ok(commit_to_json(&c))),
        Err(e) => json_response(500, err(&e.to_string())),
    }
}

pub(crate) fn handle_branch_create(runtime: &RedDBRuntime, body: Vec<u8>) -> HttpResponse {
    let body = match parse_body(body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let input = match parse_create_branch_input(&body) {
        Ok(v) => v,
        Err(e) => return json_response(400, err(&e.to_string())),
    };
    match use_cases(runtime).branch_create(input) {
        Ok(r) => json_response(200, ok(ref_to_json(&r))),
        Err(e) => json_response(500, err(&e.to_string())),
    }
}

pub(crate) fn handle_branch_list(runtime: &RedDBRuntime) -> HttpResponse {
    match use_cases(runtime).branch_list() {
        Ok(refs) => json_response(200, ok(refs_to_json(&refs))),
        Err(e) => json_response(500, err(&e.to_string())),
    }
}

pub(crate) fn handle_branch_delete(runtime: &RedDBRuntime, name: &str) -> HttpResponse {
    match use_cases(runtime).branch_delete(name) {
        Ok(()) => json_response(200, ok(JsonValue::Null)),
        Err(e) => json_response(500, err(&e.to_string())),
    }
}

pub(crate) fn handle_tag_create(runtime: &RedDBRuntime, body: Vec<u8>) -> HttpResponse {
    let body = match parse_body(body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let input = match parse_create_tag_input(&body) {
        Ok(v) => v,
        Err(e) => return json_response(400, err(&e.to_string())),
    };
    match use_cases(runtime).tag(input) {
        Ok(r) => json_response(200, ok(ref_to_json(&r))),
        Err(e) => json_response(500, err(&e.to_string())),
    }
}

pub(crate) fn handle_tag_list(runtime: &RedDBRuntime) -> HttpResponse {
    match use_cases(runtime).tag_list() {
        Ok(refs) => json_response(200, ok(refs_to_json(&refs))),
        Err(e) => json_response(500, err(&e.to_string())),
    }
}

pub(crate) fn handle_checkout(runtime: &RedDBRuntime, body: Vec<u8>) -> HttpResponse {
    let body = match parse_body(body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let input = match parse_checkout_input(&body) {
        Ok(v) => v,
        Err(e) => return json_response(400, err(&e.to_string())),
    };
    match use_cases(runtime).checkout(input) {
        Ok(r) => json_response(200, ok(ref_to_json(&r))),
        Err(e) => json_response(500, err(&e.to_string())),
    }
}

pub(crate) fn handle_merge(runtime: &RedDBRuntime, body: Vec<u8>) -> HttpResponse {
    let body = match parse_body(body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let input = match parse_merge_input(&body) {
        Ok(v) => v,
        Err(e) => return json_response(400, err(&e.to_string())),
    };
    match use_cases(runtime).merge(input) {
        Ok(outcome) => json_response(200, ok(merge_outcome_to_json(&outcome))),
        Err(e) => json_response(500, err(&e.to_string())),
    }
}

pub(crate) fn handle_reset(runtime: &RedDBRuntime, body: Vec<u8>) -> HttpResponse {
    let body = match parse_body(body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let input = match parse_reset_input(&body) {
        Ok(v) => v,
        Err(e) => return json_response(400, err(&e.to_string())),
    };
    match use_cases(runtime).reset(input) {
        Ok(()) => json_response(200, ok(JsonValue::Null)),
        Err(e) => json_response(500, err(&e.to_string())),
    }
}

pub(crate) fn handle_log(runtime: &RedDBRuntime, body: Vec<u8>) -> HttpResponse {
    let body = match parse_body(body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let input = match parse_log_input(&body) {
        Ok(v) => v,
        Err(e) => return json_response(400, err(&e.to_string())),
    };
    match use_cases(runtime).log(input) {
        Ok(commits) => json_response(200, ok(commits_to_json(&commits))),
        Err(e) => json_response(500, err(&e.to_string())),
    }
}

pub(crate) fn handle_diff(runtime: &RedDBRuntime, body: Vec<u8>) -> HttpResponse {
    let body = match parse_body(body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let input = match parse_diff_input(&body) {
        Ok(v) => v,
        Err(e) => return json_response(400, err(&e.to_string())),
    };
    match use_cases(runtime).diff(input) {
        Ok(d) => json_response(200, ok(diff_to_json(&d))),
        Err(e) => json_response(500, err(&e.to_string())),
    }
}

pub(crate) fn handle_status(runtime: &RedDBRuntime, body: Vec<u8>) -> HttpResponse {
    let body = match parse_body(body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let input = match parse_status_input(&body) {
        Ok(v) => v,
        Err(e) => return json_response(400, err(&e.to_string())),
    };
    match use_cases(runtime).status(input) {
        Ok(s) => json_response(200, ok(status_to_json(&s))),
        Err(e) => json_response(500, err(&e.to_string())),
    }
}

pub(crate) fn handle_lca(
    runtime: &RedDBRuntime,
    query: &std::collections::BTreeMap<String, String>,
) -> HttpResponse {
    let a = match query.get("a") {
        Some(s) => s.as_str(),
        None => return json_response(400, err("missing `a` query param")),
    };
    let b = match query.get("b") {
        Some(s) => s.as_str(),
        None => return json_response(400, err("missing `b` query param")),
    };
    match use_cases(runtime).lca(a, b) {
        Ok(hash) => json_response(200, ok(commit_hash_to_json(hash.as_ref()))),
        Err(e) => json_response(500, err(&e.to_string())),
    }
}

pub(crate) fn handle_conflicts_list(
    runtime: &RedDBRuntime,
    merge_state_id: &str,
) -> HttpResponse {
    match use_cases(runtime).conflicts_list(merge_state_id) {
        Ok(list) => json_response(200, ok(conflicts_to_json(&list))),
        Err(e) => json_response(500, err(&e.to_string())),
    }
}
