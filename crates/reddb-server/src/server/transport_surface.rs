//! Per-transport surface inventories for the command-coverage artifact.
//!
//! An id-level join across transports is impossible until every adapter
//! registers against the command catalog (Spec #2109 migration slices). Until
//! then each transport contributes its own inventory, and every inventory is
//! derived from that transport's real source so any dispatcher change moves the
//! generated matrix:
//!
//! - gRPC: the `service RedDb` block of
//!   `crates/reddb-grpc-proto/proto/reddb.proto`.
//! - MCP: the [`crate::mcp::tools::all_tools`] registry, cross-checked against
//!   the `handle_tools_call` dispatch arms in `crates/reddb-server/src/mcp/server.rs`.
//! - stdio: the `dispatch_method` and `dispatch_method_remote` method tables in
//!   `crates/reddb-server/src/rpc_stdio.rs`.
//! - RedWire: the `handle_session` frame-kind dispatch in
//!   `crates/reddb-server/src/wire/redwire/session.rs`.
//!
//! The Rust sources are read with `include_str!` rather than reflection because
//! the dispatchers are private free functions with no constructible handle.

const PROTO_SOURCE: &str = include_str!("../../../reddb-grpc-proto/proto/reddb.proto");
const STDIO_SOURCE: &str = include_str!("../rpc_stdio.rs");
const MCP_SERVER_SOURCE: &str = include_str!("../mcp/server.rs");
const REDWIRE_SESSION_SOURCE: &str = include_str!("../wire/redwire/session.rs");

/// Tokens that would appear in the MCP tool-call dispatcher if a tool call were
/// gated on an authorization decision. None of them are present today: the MCP
/// server executes every tool with the host process's privileges (the zero-auth
/// defect tracked by Spec #2109), and the matrix must say so.
const AUTHORIZATION_MARKERS: [&str; 5] = [
    "authorize(",
    "CommandAuthorizer",
    "require_auth",
    "AuthContext",
    "auth_required",
];

/// The stdio method name prefix whose arms are declared locally only to reject
/// the call: embedded stdio has no auth backend, so `auth.*` is excluded from
/// the local-versus-remote comparison.
pub(crate) const STDIO_AUTH_PREFIX: &str = "auth.";

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

/// gRPC rpc names declared on the canonical `service RedDb`.
pub(crate) fn grpc_rpc_methods() -> Vec<&'static str> {
    section(PROTO_SOURCE, "service RedDb {", "\n}")
        .lines()
        .filter_map(|line| line.trim().strip_prefix("rpc "))
        .filter_map(|rest| rest.split('(').next())
        .map(str::trim)
        .collect()
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

/// The `reddb.ask` tool is dispatched through a constant rather than a literal
/// arm and is appended to `tools/list` outside the registry.
pub(crate) const MCP_ASK_TOOL: &str = crate::runtime::ai::mcp_ask_tool::TOOL_NAME;

/// MCP tools advertised by `tools/list`: the static registry plus the ask tool.
pub(crate) fn mcp_advertised_tools() -> Vec<&'static str> {
    let mut tools: Vec<&'static str> = crate::mcp::tools::all_tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    tools.push(MCP_ASK_TOOL);
    tools
}

/// MCP tool names with a literal arm in `handle_tools_call`.
pub(crate) fn mcp_dispatch_tools() -> Vec<&'static str> {
    let table = section(
        MCP_SERVER_SOURCE,
        "let result = match name {",
        "\n            _ => Err(",
    );
    match_arm_literals(table, "            ")
}

/// Authorization markers found in the MCP tool-call dispatcher. Empty means
/// every tool runs unauthenticated.
pub(crate) fn mcp_authorization_markers() -> Vec<&'static str> {
    let body = section(MCP_SERVER_SOURCE, "fn handle_tools_call(", "\n    }\n");
    AUTHORIZATION_MARKERS
        .into_iter()
        .filter(|marker| body.contains(marker))
        .collect()
}

/// RedWire `MessageKind`s with a dispatch arm in `handle_session`.
pub(crate) fn redwire_frame_kinds() -> Vec<&'static str> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Pinned so adding or removing an rpc moves this number. Renames need no
    /// pin: the proto is the compile-time service contract, so a renamed rpc
    /// fails the build before this test runs.
    const GRPC_RPC_COUNT: usize = 136;
    /// Pinned so that registering or dropping an MCP tool moves this number.
    const MCP_TOOL_COUNT: usize = 41;

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

    /// The full RedWire client-to-server dispatch surface, pinned by name for
    /// the same reason as the stdio tables.
    const REDWIRE_KINDS: [&str; 19] = [
        "Bye",
        "Ping",
        "Query",
        "QueryWithParams",
        "BulkInsert",
        "BulkInsertBinary",
        "BulkInsertPrevalidated",
        "QueryBinary",
        "BulkStreamStart",
        "BulkStreamRows",
        "BulkStreamCommit",
        "Prepare",
        "ExecutePrepared",
        "Get",
        "Delete",
        "OpenStream",
        "QueueWaitOpen",
        "StreamChunk",
        "StreamCancel",
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
    fn grpc_surface_comes_from_the_proto_service_definition() {
        let methods = grpc_rpc_methods();

        assert_eq!(methods.len(), GRPC_RPC_COUNT);
        assert_eq!(methods.first(), Some(&"Health"));
        assert!(methods.contains(&"Query"));
        assert!(methods.contains(&"ExecutePrepared"));
        assert!(
            methods.iter().all(|m| !m.is_empty() && !m.contains(' ')),
            "rpc extraction picked up a non-method line: {methods:?}"
        );
    }

    #[test]
    fn mcp_dispatch_matches_the_advertised_tool_registry() {
        let advertised = mcp_advertised_tools();
        let dispatched = mcp_dispatch_tools();

        assert_eq!(advertised.len(), MCP_TOOL_COUNT);
        // Every advertised tool but the ask tool has a literal dispatch arm.
        let mut expected: Vec<&str> = advertised
            .iter()
            .copied()
            .filter(|tool| *tool != MCP_ASK_TOOL)
            .collect();
        expected.sort_unstable();
        let mut actual = dispatched.clone();
        actual.sort_unstable();
        assert_eq!(actual, expected);
        assert!(
            MCP_SERVER_SOURCE.contains("mcp_ask_tool::TOOL_NAME => self.tool_ask(args)"),
            "the ask tool lost its dispatch arm"
        );
    }

    #[test]
    fn every_mcp_tool_runs_unauthenticated() {
        assert_eq!(
            mcp_authorization_markers(),
            Vec::<&str>::new(),
            "MCP tool dispatch gained an authorization gate; the matrix's \
             auth=none rows are now wrong"
        );
    }

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
    fn redwire_surface_comes_from_the_session_dispatch() {
        let kinds = redwire_frame_kinds();

        assert_eq!(kinds, REDWIRE_KINDS.to_vec());
    }
}
