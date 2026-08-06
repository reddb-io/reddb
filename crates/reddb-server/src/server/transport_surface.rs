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
//! - RedWire: `redwire_command_id` in
//!   `crates/reddb-server/src/wire/redwire/session.rs`, which is exhaustive
//!   over [`MessageKind`].
//! - stdio: `crates/reddb-server/src/rpc_stdio.rs` declares no command ids at
//!   all, so its inventory is the empty set and the matrix reports its dispatch
//!   tables as an unjoined surface. See [`stdio_command_ids`].

use std::collections::BTreeSet;

use reddb_wire::redwire::MessageKind;

use crate::wire::redwire::session::redwire_command_id;

const STDIO_SOURCE: &str = include_str!("../rpc_stdio.rs");

/// The stdio method name prefix whose arms are declared locally only to reject
/// the call: embedded stdio has no auth backend, so `auth.*` is excluded from
/// the local-versus-remote comparison.
pub(crate) const STDIO_AUTH_PREFIX: &str = "auth.";

/// Tokens that would appear in `rpc_stdio.rs` if the stdio dispatcher were
/// bound to the command catalog. [`stdio_command_ids`] is empty only while none
/// of them is present.
const STDIO_CATALOG_MARKERS: [&str; 3] = ["command_id", "CommandAuthorizer", "command_catalog"];

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

/// Catalog commands reachable over RedWire, from the exhaustive frame-kind
/// binding.
pub(crate) fn redwire_command_ids() -> BTreeSet<&'static str> {
    (u8::MIN..=u8::MAX)
        .filter_map(MessageKind::from_u8)
        .map(redwire_command_id)
        .collect()
}

/// Catalog commands reachable over stdio: none.
///
/// `rpc_stdio.rs` dispatches on bare JSON-RPC method names and never names a
/// catalog command, so there is nothing to join and every stdio cell is an
/// honest coverage gap. `stdio_is_unbound_to_the_command_catalog` fails the
/// moment that stops being true, which forces the real join to be written
/// rather than letting this empty set quietly under-report the surface.
pub(crate) fn stdio_command_ids() -> BTreeSet<&'static str> {
    BTreeSet::new()
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

/// stdio JSON-RPC methods served by the remote (`grpc://` proxy) backend.
pub(crate) fn stdio_remote_methods() -> Vec<&'static str> {
    stdio_methods("fn dispatch_method_remote(")
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

    /// The full stdio remote (gRPC proxy) table.
    const STDIO_REMOTE_METHODS: [&str; 8] = [
        "version",
        "health",
        "query",
        "insert",
        "bulk_insert",
        "get",
        "delete",
        "close",
    ];

    /// The documented stdio remote gap: session-scoped transaction, cursor and
    /// prepared-statement methods have no place to park state across a gRPC
    /// hop, so 8 of the 16 non-auth local methods vanish in remote mode.
    const STDIO_REMOTE_GAP: [&str; 8] = [
        "tx.begin",
        "tx.commit",
        "query.open",
        "query.next",
        "query.close",
        "tx.rollback",
        "prepare",
        "execute_prepared",
    ];

    #[test]
    fn stdio_remote_mode_drops_exactly_the_documented_eight_methods() {
        let local = stdio_local_methods();
        let remote = stdio_remote_methods();

        assert_eq!(local, STDIO_LOCAL_METHODS.to_vec());
        assert_eq!(remote, STDIO_REMOTE_METHODS.to_vec());
        assert_eq!(stdio_remote_gap(), STDIO_REMOTE_GAP.to_vec());
        assert!(
            remote.iter().all(|method| local.contains(method)),
            "remote mode serves a method the local table does not declare"
        );
    }

    #[test]
    fn stdio_is_unbound_to_the_command_catalog() {
        let found: Vec<&str> = STDIO_CATALOG_MARKERS
            .into_iter()
            .filter(|marker| STDIO_SOURCE.contains(marker))
            .collect();

        assert_eq!(
            found,
            Vec::<&str>::new(),
            "rpc_stdio.rs gained a command binding; stdio_command_ids must now \
             report it instead of returning the empty set"
        );
        assert!(stdio_command_ids().is_empty());
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
