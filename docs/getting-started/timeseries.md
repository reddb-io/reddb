# Time-Series Quickstart

Use this when points are ordered by event time. The Collection is the universal
container; the time-series model is the semantic layer.

Start RedDB:

```bash
docker run --rm -p 5000:5000 ghcr.io/reddb-io/reddb:latest
```

Or open an embedded runtime and run the same SQL.

```sql quickstart
CREATE TIMESERIES cpu_metrics RETENTION 7 d;
INSERT INTO cpu_metrics (metric, value, tags, timestamp) VALUES ('cpu', 10.0, {host: 'api-1'}, 1704067200000000000), ('cpu', 20.0, {host: 'api-1'}, 1704067260000000000);
SELECT COUNT(*) AS points FROM cpu_metrics;
```

First meaningful result: the final query confirms the ingested points.

Where to go next: [Time-Series](/data-models/timeseries.md),
[Hypertables](/data-models/hypertables.md), and [INSERT](/query/insert.md).
