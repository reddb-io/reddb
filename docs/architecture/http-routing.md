# HTTP Routing Architecture

RedDB's HTTP server is moving toward a route catalog instead of one large
match statement. The catalog is inspired by Raffel's filesystem discovery, but
uses build-time Rust discovery so production routing stays static and
auditable.

## Route Files

Route metadata lives beside the route family under:

```text
crates/reddb-server/src/server/routes/**/*.route.rs
```

Each discovered file exposes:

```rust
pub(crate) fn register(registry: &mut RouteRegistry)
```

`crates/reddb-server/build.rs` scans those files and generates the registry
module in `OUT_DIR`. This gives us filesystem organization without runtime
dynamic imports.

The first migrated family is the lifecycle health trio:

```text
GET /health/live
GET /health/ready
GET /health/startup
```

Those routes now use catalog metadata for listener-surface checks, public auth,
quota bypass, and buffered dispatch. Non-migrated routes continue through the
legacy router until their families move.

The catalog also inventories the current public route contract for these
families:

```text
auth
query / streams
metrics / prometheus
admin / ops
catalog
graph
repo
physical
ai
```

For those families, route files now declare method, current live path,
audience, auth class, listener surfaces, middleware intent, and selected
canonical `/v1/*` aliases. Dispatch migration is intentionally incremental:
route metadata lands first, then each family moves off the legacy matcher once
its auth and compatibility behavior is covered by tests.

## Catalog Contract

Every route must declare:

- `id`: stable metric/docs identifier, never raw path.
- `method` and `pattern`.
- `family`: product family such as `auth`, `query`, `catalog`, `metrics`.
- `audience`: client, operator, infra, compatibility adapter, or internal.
- `auth`: public, user auth, admin token, ops capability, or stream lease.
- `surfaces`: which listeners may serve it: public, admin, metrics.
- `stability`: stable, compatibility, deprecated, or internal.
- `middlewares`: route-local policy chain.
- `aliases`: intentional compatibility paths with a reason.

## Matching Rules

- Exact routes win over dynamic routes.
- Dynamic route ambiguity fails catalog build/tests instead of relying on
  registration order.
- Terminal wildcards are allowed, but overlapping dynamic routes are rejected.
- Optional path params are not supported; define both explicit routes.
- Trailing slash remains significant.

## Route Taxonomy

Native RedDB APIs should converge under `/v1/*` by product domain:

```text
/v1/auth/*
/v1/query
/v1/query/stream
/v1/streams/input
/v1/catalog/*
/v1/admin/*
/v1/ops/*
/v1/ai/*
/v1/graph/*
/v1/repo/*
/v1/config/*
/v1/vault/*
/v1/kv/*
```

Infrastructure and protocol endpoints may stay short when external systems
expect them:

```text
/health/*
/ready/*
/metrics
/redwire
```

Prometheus/Grafana compatibility remains an adapter surface. Keep `/api/v1/*`
where Grafana and Prometheus tooling expect it, and model any RedDB-native
metrics API separately under `/v1`.
