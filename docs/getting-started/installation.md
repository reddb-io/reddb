# Installation

RedDB ships as a single binary called `red`. There are several ways to install it.

## Quick Install (Script)

The fastest way to install the latest stable release:

```bash
curl -fsSL https://raw.githubusercontent.com/forattini-dev/reddb/main/install.sh | bash
```

For pre-release (next channel):

```bash
curl -fsSL https://raw.githubusercontent.com/forattini-dev/reddb/main/install.sh | bash -s -- --channel next
```

## Build from Source

RedDB requires Rust 1.75+ and the `protoc` protobuf compiler.

```bash
# Clone the repository
git clone https://github.com/forattini-dev/reddb.git
cd reddb

# Build the release binary
cargo build --release --bin red

# Install to system path
sudo install -m 0755 target/release/red /usr/local/bin/red
```

Verify the installation:

```bash
red version
```

## Cargo (as a Library)

To use RedDB as an embedded database in your Rust project:

```toml
[dependencies]
reddb = "0.1"
```

Optional feature flags:

| Feature | Description |
|:--------|:------------|
| `query-vector` | Enable vector similarity queries |
| `query-graph` | Enable graph traversal and analytics queries |
| `query-fulltext` | Enable full-text search |
| `encryption` | Enable AES-256-GCM encryption at rest |
| `backend-s3` | Enable S3-compatible remote storage |
| `backend-turso` | Enable Turso (libSQL) as a remote backend |
| `backend-d1` | Enable Cloudflare D1 as a remote backend |

Example with features enabled:

```toml
[dependencies]
reddb = { version = "0.1", features = ["query-vector", "query-graph", "encryption"] }
```

## Docker

Pull and run the pre-built image:

```bash
docker build -t reddb .
```

Run an HTTP server:

```bash
docker run --rm -it \
  -p 8080:8080 \
  -v $(pwd)/data:/data \
  reddb red server --http --path /data/reddb.rdb --bind 0.0.0.0:8080
```

Run a gRPC server:

```bash
docker run --rm -it \
  -p 50051:50051 \
  -v $(pwd)/data:/data \
  reddb red server --grpc --path /data/reddb.rdb --bind 0.0.0.0:50051
```

See [Docker Deployment](/deployment/docker.md) for production configurations.

## Systemd Service (Linux)

Install as a system service that auto-starts on boot:

```bash
sudo ./scripts/install-systemd-service.sh \
  --binary /usr/local/bin/red \
  --grpc \
  --path /var/lib/reddb/data.rdb \
  --bind 0.0.0.0:50051
```

This configures `Restart=always` and `systemctl enable` for the unit.

## Verify Installation

```bash
# Check version
red version

# Start an in-memory server for testing
red server --http --bind 127.0.0.1:8080

# In another terminal, check health
curl http://127.0.0.1:8080/health
```

> [!TIP]
> For development, omit the `--path` flag to run entirely in-memory. No files are created.
