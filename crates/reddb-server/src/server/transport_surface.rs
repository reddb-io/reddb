//! Per-transport inventories for the command-coverage artifact.
//!
//! Every inventory is derived from that transport's own source of truth, so a
//! dispatcher change moves the generated matrix instead of leaving it stale:
//!
//! - gRPC: the rpc-to-command binding table in
//!   [`crate::grpc::catalog_dispatch`], itself pinned against the `service
//!   RedDb` block of `crates/reddb-grpc-proto/proto/reddb.proto`.
//! - MCP: the [`crate::mcp::tools::all_tools`] registry plus the `reddb.ask`
//!   binding that lives outside it.
//! - RedWire: the real `handle_session` dispatch arms, mapped through
//!   `redwire_command_id` in
//!   `crates/reddb-server/src/wire/redwire/session.rs`.
//! - stdio: the catalog-backed method table in
//!   [`crate::rpc_stdio::stdio_method_catalog`], including each remote
//!   disposition.

use std::collections::BTreeSet;

use reddb_wire::redwire::MessageKind;

use crate::wire::redwire::session::redwire_command_id;

const STDIO_SOURCE: &str = include_str!("../rpc_stdio.rs");
const REDWIRE_SESSION_SOURCE: &str = include_str!("../wire/redwire/session.rs");

const REDWIRE_DISPATCH_KINDS: [MessageKind; 19] = [
    MessageKind::Bye,
    MessageKind::Ping,
    MessageKind::Query,
    MessageKind::QueryWithParams,
    MessageKind::BulkInsert,
    MessageKind::BulkInsertBinary,
    MessageKind::BulkInsertPrevalidated,
    MessageKind::QueryBinary,
    MessageKind::BulkStreamStart,
    MessageKind::BulkStreamRows,
    MessageKind::BulkStreamCommit,
    MessageKind::Prepare,
    MessageKind::ExecutePrepared,
    MessageKind::Get,
    MessageKind::Delete,
    MessageKind::OpenStream,
    MessageKind::QueueWaitOpen,
    MessageKind::StreamChunk,
    MessageKind::StreamCancel,
];

/// The stdio method name prefix whose arms are declared locally only to reject
/// the call: embedded stdio has no auth backend, so `auth.*` is excluded from
/// the local-versus-remote comparison.
pub(crate) const STDIO_AUTH_PREFIX: &str = "auth.";

/// Catalog commands reachable over gRPC.
pub(crate) fn grpc_command_ids() -> BTreeSet<&'static str> {
    crate::grpc::catalog_dispatch::bound_command_ids().collect()
}

/// Catalog commands reachable over MCP, from the advertised tool registry.
pub(crate) fn mcp_command_ids() -> BTreeSet<&'static str> {
    crate::mcp::tools::all_tools()
        .into_iter()
        .map(|tool| tool.command_id)
        .chain(std::iter::once(crate::mcp::tools::ASK_TOOL_COMMAND_ID))
        .collect()
}

/// Catalog commands reachable over RedWire, from the real frame dispatch plus
/// the handshake-only `auth.login` command.
pub(crate) fn redwire_command_ids() -> BTreeSet<&'static str> {
    REDWIRE_DISPATCH_KINDS
        .iter()
        .copied()
        .filter_map(redwire_command_id)
        // `auth.login` is handled by `perform_handshake`, before the frame
        // dispatch loop.
        .chain(std::iter::once("auth.login"))
        .collect()
}

/// RedWire `MessageKind`s with a real dispatch arm in `handle_session`.
fn redwire_frame_kinds() -> Vec<&'static str> {
    section(
        REDWIRE_SESSION_SOURCE,
        "match frame.kind {",
        "\n            other => {",
    )
    .lines()
    .filter_map(|line| line.strip_prefix("            MessageKind::"))
    .filter_map(|rest| rest.split_once(" => "))
    .map(|(kind, _)| kind)
    .collect()
}

/// Catalog commands with an explicit stdio disposition.
pub(crate) fn stdio_command_ids() -> BTreeSet<&'static str> {
    crate::rpc_stdio::stdio_method_catalog()
        .iter()
        .map(|entry| entry.command_id)
        .collect()
}

/// Slice `source` between the first `start` anchor and the first `end`
/// terminator after it. Anchors are load-bearing: a miss means the transport
/// moved and the inventory would silently go empty, so it panics instead.
fn after<'a>(source: &'a str, marker: &str) -> &'a str {
    let from = source
        .find(marker)
        .unwrap_or_else(|| panic!("transport surface anchor not found: {marker}"));
    &source[from + marker.len()..]
}

fn section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let rest = after(source, start);
    let to = rest
        .find(end)
        .unwrap_or_else(|| panic!("transport surface terminator not found: {end}"));
    &rest[..to]
}

/// Collect the string-literal patterns of a `match` table. Only lines at
/// exactly `indent` are considered, so string literals nested inside arm bodies
/// are not mistaken for arms. Handles both `"a" => …` and or-patterns split
/// across lines (`"a"` / `| "b" => …`).
fn match_arm_literals(table: &'static str, indent: &str) -> Vec<&'static str> {
    let mut names = Vec::new();
    for line in table.lines() {
        let Some(rest) = line.strip_prefix(indent) else {
            continue;
        };
        if rest.starts_with(' ') {
            continue;
        }
        let rest = rest.strip_prefix("| ").unwrap_or(rest);
        let Some(rest) = rest.strip_prefix('"') else {
            continue;
        };
        let Some(close) = rest.find('"') else {
            continue;
        };
        let tail = rest[close + 1..].trim();
        if !tail.is_empty() && !tail.starts_with("=>") {
            continue;
        }
        names.push(&rest[..close]);
    }
    names
}

fn stdio_methods(dispatcher: &str) -> Vec<&'static str> {
    let body = section(STDIO_SOURCE, dispatcher, "\n        other =>");
    let table = after(body, "match method {");
    match_arm_literals(table, "        ")
}

/// stdio JSON-RPC methods served by the embedded (`memory://`, `file://`) backend.
pub(crate) fn stdio_local_methods() -> Vec<&'static str> {
    stdio_methods("fn dispatch_method(")
}

/// stdio JSON-RPC methods with a remote (`grpc://` proxy) disposition.
pub(crate) fn stdio_remote_methods() -> Vec<&'static str> {
    crate::rpc_stdio::stdio_method_catalog()
        .iter()
        .map(|entry| entry.method)
        .collect()
}

/// stdio JSON-RPC methods implemented by the remote backend.
pub(crate) fn stdio_remote_served_methods() -> Vec<&'static str> {
    crate::rpc_stdio::stdio_method_catalog()
        .iter()
        .filter_map(|entry| {
            matches!(
                entry.remote,
                crate::rpc_stdio::StdioRemoteDisposition::Served
            )
            .then_some(entry.method)
        })
        .collect()
}

/// stdio JSON-RPC methods explicitly rejected by the remote backend.
pub(crate) fn stdio_remote_unsupported_methods() -> Vec<(&'static str, &'static str)> {
    crate::rpc_stdio::stdio_method_catalog()
        .iter()
        .filter_map(|entry| match entry.remote {
            crate::rpc_stdio::StdioRemoteDisposition::Served => None,
            crate::rpc_stdio::StdioRemoteDisposition::Unsupported { error_code } => {
                Some((entry.method, error_code))
            }
        })
        .collect()
}

/// Locally served stdio methods that have no remote-mode arm. `auth.*` is
/// excluded because those arms exist locally only to return an error.
pub(crate) fn stdio_remote_gap() -> Vec<&'static str> {
    let remote = stdio_remote_methods();
    stdio_local_methods()
        .into_iter()
        .filter(|method| !method.starts_with(STDIO_AUTH_PREFIX))
        .filter(|method| !remote.contains(method))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc_stdio::error_code;

    #[test]
    fn redwire_catalog_bindings_have_real_dispatch() {
        let dispatched = redwire_frame_kinds();
        let expected: Vec<String> = REDWIRE_DISPATCH_KINDS
            .iter()
            .map(|kind| format!("{kind:?}"))
            .collect();
        assert_eq!(dispatched, expected);

        let reachable_ids: BTreeSet<&str> = REDWIRE_DISPATCH_KINDS
            .into_iter()
            .filter_map(redwire_command_id)
            .chain(std::iter::once("auth.login"))
            .collect();
        assert_eq!(redwire_command_ids(), reachable_ids);

        let missing: Vec<MessageKind> = (u8::MIN..=u8::MAX)
            .filter_map(MessageKind::from_u8)
            .filter(|kind| redwire_command_id(*kind).is_some_and(|id| !reachable_ids.contains(id)))
            .collect();
        assert!(
            missing.is_empty(),
            "redwire_command_id binds commands without a real dispatch arm: {missing:?}"
        );
    }

    /// The full stdio local table. Pinned by name, not just by count, because
    /// renaming an arm keeps the count intact.
    const STDIO_LOCAL_METHODS: [&str; 21] = [
        "tx.begin",
        "tx.commit",
        "query.open",
        "query.next",
        "query.close",
        "tx.rollback",
        "version",
        "health",
        "query",
        "prepare",
        "execute_prepared",
        "insert",
        "bulk_insert",
        "get",
        "delete",
        "close",
        "auth.login",
        "auth.whoami",
        "auth.change_password",
        "auth.create_api_key",
        "auth.revoke_api_key",
    ];

    const STDIO_REMOTE_DISPOSITIONS: [(&str, &str, crate::rpc_stdio::StdioRemoteDisposition); 16] = [
        (
            "tx.begin",
            "query.execute",
            unsupported(error_code::TX_NOT_SUPPORTED_REMOTE),
        ),
        (
            "tx.commit",
            "query.execute",
            unsupported(error_code::TX_NOT_SUPPORTED_REMOTE),
        ),
        (
            "query.open",
            "streams.query.output",
            unsupported(error_code::CURSOR_NOT_SUPPORTED_REMOTE),
        ),
        (
            "query.next",
            "streams.query.output",
            unsupported(error_code::CURSOR_NOT_SUPPORTED_REMOTE),
        ),
        (
            "query.close",
            "streams.query.cancel",
            unsupported(error_code::CURSOR_NOT_SUPPORTED_REMOTE),
        ),
        (
            "tx.rollback",
            "query.execute",
            unsupported(error_code::TX_NOT_SUPPORTED_REMOTE),
        ),
        (
            "version",
            "health.live",
            crate::rpc_stdio::StdioRemoteDisposition::Served,
        ),
        (
            "health",
            "ops.health.aggregate",
            crate::rpc_stdio::StdioRemoteDisposition::Served,
        ),
        (
            "query",
            "query.execute",
            crate::rpc_stdio::StdioRemoteDisposition::Served,
        ),
        (
            "prepare",
            "query.execute",
            unsupported(error_code::PREPARED_NOT_SUPPORTED_REMOTE),
        ),
        (
            "execute_prepared",
            "query.execute",
            unsupported(error_code::PREPARED_NOT_SUPPORTED_REMOTE),
        ),
        (
            "insert",
            "collections.rows.create",
            crate::rpc_stdio::StdioRemoteDisposition::Served,
        ),
        (
            "bulk_insert",
            "collections.bulk.rows",
            crate::rpc_stdio::StdioRemoteDisposition::Served,
        ),
        (
            "get",
            "collections.entities.get",
            crate::rpc_stdio::StdioRemoteDisposition::Served,
        ),
        (
            "delete",
            "collections.entities.delete",
            crate::rpc_stdio::StdioRemoteDisposition::Served,
        ),
        (
            "close",
            "health.live",
            crate::rpc_stdio::StdioRemoteDisposition::Served,
        ),
    ];

    const fn unsupported(error_code: &'static str) -> crate::rpc_stdio::StdioRemoteDisposition {
        crate::rpc_stdio::StdioRemoteDisposition::Unsupported { error_code }
    }

    #[test]
    fn every_stdio_method_has_a_remote_disposition() {
        let local = stdio_local_methods();
        let remote = stdio_remote_methods();
        let dispositions: Vec<_> = crate::rpc_stdio::stdio_method_catalog()
            .iter()
            .map(|entry| (entry.method, entry.command_id, entry.remote))
            .collect();

        assert_eq!(local, STDIO_LOCAL_METHODS.to_vec());
        assert_eq!(dispositions, STDIO_REMOTE_DISPOSITIONS.to_vec());
        assert_eq!(remote.len(), STDIO_REMOTE_DISPOSITIONS.len());
        assert!(stdio_remote_gap().is_empty());
        assert!(
            remote.iter().all(|method| local.contains(method)),
            "remote mode serves a method the local table does not declare"
        );
    }

    #[test]
    fn stdio_is_bound_to_the_command_catalog() {
        assert!(!stdio_command_ids().is_empty());
    }

    /// Every id an adapter claims to serve must exist in the catalog, or the
    /// matrix would silently drop that adapter's coverage: the join is keyed on
    /// the catalog's rows.
    #[test]
    fn every_transport_inventory_names_catalogued_commands() {
        let catalog = crate::server::command_catalog();
        let inventories = [
            ("gRPC", grpc_command_ids()),
            ("MCP", mcp_command_ids()),
            ("stdio", stdio_command_ids()),
            ("RedWire", redwire_command_ids()),
        ];

        for (transport, ids) in inventories {
            assert!(!ids.is_empty(), "{transport} inventory is empty");
            for id in ids {
                assert!(
                    catalog.command(id).is_some(),
                    "{transport} binds unknown command {id}"
                );
            }
        }
    }
}
