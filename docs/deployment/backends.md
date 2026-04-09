# Remote Backends (S3, R2, Turso, D1)

RedDB supports remote storage backends for serverless and distributed deployments.

## Available Backends

| Backend | Feature Flag | Description |
|:--------|:-------------|:------------|
| S3 | `backend-s3` | AWS S3 or S3-compatible storage (MinIO, R2) |
| Turso | `backend-turso` | Turso (libSQL) remote database |
| D1 | `backend-d1` | Cloudflare D1 edge database |

## Enabling

```toml
[dependencies]
reddb = { version = "0.1", features = ["backend-s3", "backend-turso"] }
```

## S3 / R2

Store the database file on S3-compatible object storage:

```bash
red server --http \
  --path s3://my-bucket/reddb/data.rdb \
  --bind 0.0.0.0:8080
```

Compatible with:
- AWS S3
- Cloudflare R2
- MinIO
- DigitalOcean Spaces

## Turso

Use Turso (libSQL) as the storage backend:

```bash
red server --http \
  --path turso://my-db.turso.io \
  --bind 0.0.0.0:8080
```

## Cloudflare D1

For edge deployments on Cloudflare Workers:

```bash
red server --http \
  --path d1://my-database-id \
  --bind 0.0.0.0:8080
```

## Use Cases

| Backend | Best For |
|:--------|:---------|
| Local file | Development, single-server production |
| S3/R2 | Serverless, backup, multi-region storage |
| Turso | Edge SQL, global distribution |
| D1 | Cloudflare Workers integration |

> [!WARNING]
> Remote backends add network latency to every storage operation. Use them for serverless/edge workloads where local disk is unavailable.
