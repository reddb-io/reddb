# Health & Observability

RedDB provides comprehensive health checks, readiness probes, and runtime statistics.

## Health Check

```bash
curl http://127.0.0.1:8080/health
```

Response:

```json
{
  "healthy": true,
  "state": "running",
  "checked_at": "2024-01-15T10:30:00Z"
}
```

Returns HTTP 200 when healthy, 503 when degraded.

When RedDB is started with HTTP enabled, health responses include transport
listener readiness:

```json
{
  "transport_listeners": {
    "active": [
      { "transport": "grpc", "bind_addr": "127.0.0.1:50051", "explicit": true }
    ],
    "failed": [
      {
        "transport": "http",
        "bind_addr": "127.0.0.1:5055",
        "explicit": false,
        "reason": "http listener bind 127.0.0.1:5055: address already in use"
      }
    ]
  }
}
```

Explicit bind addresses fail startup. Implicit/default listener failures are
reported here when another requested listener remains active.

## Readiness Probes

| Endpoint | Purpose |
|:---------|:--------|
| `GET /ready` | General readiness |
| `GET /ready/query` | Query engine ready |
| `GET /ready/write` | Write path ready |
| `GET /ready/repair` | Repair operations ready |
| `GET /ready/serverless` | Serverless readiness (all gates) |
| `GET /ready/serverless/query` | Serverless query gate |
| `GET /ready/serverless/write` | Serverless write gate |
| `GET /ready/serverless/repair` | Serverless repair gate |

## Runtime Statistics

```bash
curl http://127.0.0.1:8080/stats
```

Response:

```json
{
  "collection_count": 5,
  "total_entities": 10000,
  "total_memory_bytes": 52428800,
  "cross_ref_count": 150,
  "active_connections": 3,
  "idle_connections": 7,
  "total_checkouts": 1500,
  "paged_mode": true,
  "started_at_unix_ms": 1705312200000
}
```

`/stats` reports instance-wide totals. For **per-collection** storage telemetry
— including `on_disk_bytes` (reachable primary B-tree bytes) alongside
`in_memory_bytes`, `entities`, and `segments` — query the `red.collections`
virtual table:

```sql
SELECT name, model, entities, in_memory_bytes, on_disk_bytes FROM red.collections;
```

`on_disk_bytes` is a conservative estimate (reachable B-tree pages × the 4 KiB
page size); it is `NULL` when the local page store cannot expose a root page,
and excludes shared header/freelist/WAL bytes and unreachable artifacts. See
[`red.collections`](red-schema.md#redcollections) for the full column set and
caveats.

## Catalog Readiness

Get a comprehensive readiness view:

```bash
curl http://127.0.0.1:8080/catalog/readiness
```

This includes:
- Query/write/repair readiness gates
- Health status
- Physical authority status

## Catalog Attention

Find what needs attention:

```bash
curl http://127.0.0.1:8080/catalog/attention
```

Returns failed indexes, stale projections, and other items needing operator action.

## Catalog Consistency

```bash
curl http://127.0.0.1:8080/catalog/consistency
```

Checks that the catalog state is consistent across all subsystems.

## Physical Authority

```bash
curl http://127.0.0.1:8080/physical/authority
```

Returns the physical storage authority status, including header validity and repair needs.

## Kubernetes Integration

Use health and readiness probes in Kubernetes:

```yaml
livenessProbe:
  httpGet:
    path: /health
    port: 8080
  initialDelaySeconds: 5
  periodSeconds: 10

readinessProbe:
  httpGet:
    path: /ready
    port: 8080
  initialDelaySeconds: 5
  periodSeconds: 5
```

## Monitoring

For continuous monitoring, poll the stats endpoint:

```bash
watch -n 5 'curl -s http://127.0.0.1:8080/stats | python3 -m json.tool'
```
