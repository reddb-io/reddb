# Modes and Transports

This is the vocabulary to use across RedDB docs.

The first distinction is:

- **execution mode**: where the engine runs
- **transport**: how a client talks to that engine

If those two ideas get mixed together, terms like `binary`, `gRPC`, and `wire` become ambiguous very quickly.

## The Short Version

Use this mental model:

1. Choose **where the engine runs**: embedded in your process, or as a standalone `red` process.
2. If you run a standalone process, choose **how clients reach it**: router, HTTP, gRPC, or wire.
3. If you use a driver, check whether it talks to the server directly or goes through the local `red` binary as a bridge.

## Terms

| Term | What it is | Use it when |
|:-----|:-----------|:------------|
| `red` binary | The executable file you install and run | You want the CLI, `red server`, `red rpc --stdio`, or a launcher from npm |
| Embedded mode | The RedDB engine linked directly into your Rust process | You do not want a separate database process |
| Server mode | A standalone `red server` process | Multiple clients or services need to share the same database |
| Router | A TCP front-door that auto-detects HTTP, gRPC, and wire | You want one bind address for local development or a simple default setup |
| HTTP | JSON/REST API | You want `curl`, reverse proxies, browser-friendly tooling, or ops endpoints |
| gRPC | Protobuf RPC API over HTTP/2 | You want typed RPC clients, the richest remote API, or service-to-service traffic |
| Wire | RedDB's raw TCP binary protocol | You want a minimal, low-overhead custom client |
| `red rpc --stdio` | JSON-RPC over stdin/stdout exposed by the `red` binary | A local driver wants to talk to a child `red` process or proxy to remote gRPC |

## What `binary` Means In Practice

The word `binary` appears in four different contexts in this repo:

- `red` binary: the executable you install, ship, or call from npm
- binary wire protocol: the raw TCP framing used by `wire`
- binary bulk insert: a gRPC fast path that sends protobuf native values instead of JSON
- binary file format: the on-disk `.rdb` storage format

Those are related only by the generic English word "binary". They are not the same feature.

## Execution Modes

### 1. Embedded mode

The engine lives inside your Rust process.

There is no network hop and no standalone `red` daemon.

Use this when:

- your app is Rust
- you want the lowest latency
- you want deployment to stay process-local

Example:

```rust
use reddb::RedDB;

let db = RedDB::open("./data.rdb")?;
```

### 2. Server mode

The engine runs in a separate `red server` process.

Clients then connect through one of the remote transports:

- router
- HTTP
- gRPC
- wire

Use this when:

- multiple processes need shared access
- you want health, ops, and remote administration
- you want language-agnostic access

## Remote Access Paths

### Router

If you start `red server` without choosing explicit transports, RedDB opens a default router on `127.0.0.1:5050`.

That router inspects the incoming TCP traffic and forwards it internally to:

- HTTP
- gRPC
- wire

Use this when:

- you want the simplest default local setup
- one port is easier than remembering multiple transport ports

Example:

```bash
red server --path ./data/reddb.rdb
```

### HTTP

Use HTTP for JSON payloads, operational endpoints, and tooling that already expects REST-like APIs.

Example:

```bash
red server --http --path ./data/reddb.rdb --bind 127.0.0.1:8080
```

### gRPC

Use gRPC when you want the richest remote API surface, typed clients, or service-to-service integration.

This is also the transport used by:

- `red connect`
- replication
- the remote side of `red rpc --stdio --connect grpc://...`

Example:

```bash
red server --grpc --path ./data/reddb.rdb --bind 127.0.0.1:50051
```

### Wire

Use wire when you want the raw RedDB TCP protocol with low framing overhead and you control both sides of the client/server contract.

Example:

```bash
red server --path ./data/reddb.rdb --wire-bind 127.0.0.1:5051
```

## Where `red rpc --stdio` Fits

`red rpc --stdio` is not a new database transport like HTTP, gRPC, or wire.

It is a **local process bridge**:

- the client process speaks JSON-RPC over stdin/stdout to the `red` binary
- the `red` binary either opens a local engine in-process, or proxies to a remote gRPC server

That means `stdio` is mostly a driver integration detail, not the main remote API you expose to other services.

## Recommended Choices

| Situation | Recommended path |
|:----------|:-----------------|
| Local development, simplest setup | `red server --path ./data/reddb.rdb` and use the router on `127.0.0.1:5050` |
| `curl`, browser tooling, reverse proxy | HTTP |
| REPL, typed RPC client, replication, broad remote API | gRPC |
| Custom low-overhead client | wire |
| Rust app with no separate server | embedded mode |
| JS/TS app using the official package | let the driver manage the `red` binary unless you need a direct remote gRPC target |

## Related Docs

- [Installation](/getting-started/installation.md)
- [Connect](/getting-started/connect.md)
- [Configuration](/getting-started/configuration.md)
- [CLI Reference](/api/cli.md)
- [gRPC API](/api/grpc.md)
- [HTTP API](/api/http.md)
- [Embedded (Rust)](/api/embedded.md)
