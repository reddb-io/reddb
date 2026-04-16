# Connect to RedDB

Before choosing a client command, separate two concepts:

- **execution mode**: embedded vs standalone `red` process
- **access path**: router, HTTP, gRPC, or wire

For the full terminology, see [Modes and Transports](/getting-started/modes-and-transports.md).

The practical rules are:

- `red connect` talks to **gRPC**
- `curl` and browser-style tooling talk to **HTTP**
- custom low-level clients can talk to **wire**
- if you start `red server` without explicit transport flags, you get the default **router** on `127.0.0.1:5050`

## Simplest Local Setup: Router

Start the server with the default routed front-door:

```bash
mkdir -p ./data
red server --path ./data/reddb.rdb
```

This exposes one port:

- router on `127.0.0.1:5050`

That router auto-detects:

- HTTP traffic
- gRPC traffic
- wire traffic

Examples:

```bash
red health --bind 127.0.0.1:5050
red connect 127.0.0.1:5050   # gRPC through the router
```

## Connect over HTTP

Start an HTTP server:

```bash
mkdir -p ./data
red server --http --path ./data/reddb.rdb --bind 127.0.0.1:8080
```

Check health:

```bash
curl -s http://127.0.0.1:8080/health
```

Write a row:

```bash
curl -X POST http://127.0.0.1:8080/collections/hosts/rows \
  -H 'content-type: application/json' \
  -d '{
    "fields": {
      "ip": "10.0.0.1",
      "os": "linux",
      "critical": true
    }
  }'
```

Run a query:

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query":"SELECT * FROM hosts"}'
```

Universal query:

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query":"FROM ANY ORDER BY _score DESC LIMIT 10"}'
```

## Connect with `red connect` Over gRPC

Start a gRPC server:

```bash
mkdir -p ./data
red server --grpc --path ./data/reddb.rdb --bind 127.0.0.1:50051
```

Open the REPL:

```bash
red connect 127.0.0.1:50051
```

Run a single query and exit:

```bash
red connect --query "SELECT * FROM hosts" 127.0.0.1:50051
```

Pass an auth token:

```bash
red connect --token "$REDDB_TOKEN" 127.0.0.1:50051
```

## Connect Over Wire

Start a dedicated wire listener:

```bash
mkdir -p ./data
red server --path ./data/reddb.rdb --wire-bind 127.0.0.1:5051
```

Use wire when:

- you control the client implementation
- you want the RedDB binary TCP framing directly
- you do not need the full ergonomic surface of HTTP or gRPC

The official docs for wire are currently lighter than HTTP/gRPC. Treat it as the lower-level transport.

## Connect from `npx`

You can run the same flows through the npm package.

Start HTTP:

```bash
npx reddb-cli@latest server --http --path ./data/reddb.rdb --bind 127.0.0.1:8080
```

Start gRPC:

```bash
npx reddb-cli@latest server --grpc --path ./data/reddb.rdb --bind 127.0.0.1:50051
```

Open the REPL through the wrapper:

```bash
npx reddb-cli@latest connect 127.0.0.1:50051
```

## Connect Through the Local `red` Binary

Some drivers do not speak HTTP, gRPC, or wire directly.

Instead, they launch the local `red` binary and talk to it over stdio:

- local `memory://` or `file://` style flows can run the engine in the spawned `red` process
- remote `grpc://host:port` flows can use `red rpc --stdio --connect ...` as a bridge to a remote gRPC server

This is a driver integration detail, not a fourth remote server API.

## Embedded Mode

If you are using RedDB directly from Rust, you may not need any server transport at all.

Use embedded mode when:

- your application is Rust
- you want direct in-process calls
- you do not want a separate `red server` process

## Which one should you use?

- Use the router on `127.0.0.1:5050` when you want the simplest default local setup.
- Use HTTP when you want `curl`, browser tooling, reverse proxies, or JSON/REST-style integrations.
- Use gRPC when you want `red connect`, typed client integrations, replication, or the broadest remote API surface.
- Use wire when you are building a custom low-level client and want the raw RedDB TCP protocol.
- Use embedded mode when you do not want a separate server process at all.
