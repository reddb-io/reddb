# Connect to RedDB

RedDB can be reached in two different ways depending on how you run it:

- HTTP, for REST-style requests and operational endpoints
- gRPC, for the CLI REPL and typed RPC clients

The important distinction is simple:

- `red connect` talks to a gRPC server
- `curl` and browser-based tooling talk to an HTTP server

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

## Connect with `red connect`

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

## Connect from `npx`

You can run the same flows through the npm wrapper.

Start HTTP:

```bash
npx reddb --auto-download -- server --http --path ./data/reddb.rdb --bind 127.0.0.1:8080
```

Start gRPC:

```bash
npx reddb --auto-download -- server --grpc --path ./data/reddb.rdb --bind 127.0.0.1:50051
```

Open the REPL through the wrapper:

```bash
npx reddb --auto-download -- connect 127.0.0.1:50051
```

## Which one should you use?

- Use HTTP when you want `curl`, browser tooling, reverse proxies, or REST-style integrations.
- Use gRPC when you want `red connect`, typed client integrations, or lower-overhead service-to-service access.
- Use embedded mode when you do not want a separate server process at all.
