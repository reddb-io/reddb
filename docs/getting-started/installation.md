# Installation

RedDB ships as a single binary called `red`. In JavaScript and TypeScript, use the `reddb` driver package in application code and `reddb-cli` only as the npm CLI launcher.

## Install from GitHub Releases

### Recommended: installer script

The installer resolves the correct GitHub Release asset for your platform:

```bash
curl -fsSL https://raw.githubusercontent.com/forattini-dev/reddb/main/install.sh | bash
```

Install a specific version:

```bash
curl -fsSL https://raw.githubusercontent.com/forattini-dev/reddb/main/install.sh | bash -s -- --version v0.1.2
```

Install the prerelease channel:

```bash
curl -fsSL https://raw.githubusercontent.com/forattini-dev/reddb/main/install.sh | bash -s -- --channel next
```

Change the install location:

```bash
curl -fsSL https://raw.githubusercontent.com/forattini-dev/reddb/main/install.sh | bash -s -- --install-dir "$HOME/.local/bin"
```

Verify:

```bash
red version
```

### Manual release download

If you want to manage the binary yourself, download the asset for your OS and architecture from:

`https://github.com/forattini-dev/reddb/releases`

Then place `red` somewhere in your `PATH`:

```bash
chmod +x ./red
sudo install -m 0755 ./red /usr/local/bin/red
red version
```

## Install with `npx`

The `reddb-cli` npm package installs the real `red` binary and forwards CLI arguments directly to it.

Run a command through `npx`:

```bash
npx reddb-cli@latest version
```

Start an HTTP server through `npx`:

```bash
npx reddb-cli@latest server --http --path ./data/reddb.rdb --bind 127.0.0.1:8080
```

If you use `pnpm`:

```bash
pnpm dlx reddb-cli version
```

## Install the JavaScript / TypeScript driver

Use the `reddb` package in app code:

```bash
pnpm add reddb
```

```ts
import { connect } from 'reddb'

const db = await connect('memory://')
const result = await db.query('SELECT * FROM users LIMIT 10')
await db.close()
```

## Build from source

RedDB requires Rust and `protoc`.

```bash
git clone https://github.com/forattini-dev/reddb.git
cd reddb
cargo build --release --bin red
./target/release/red version
```

Install the built binary:

```bash
sudo install -m 0755 target/release/red /usr/local/bin/red
```

## Use as an embedded Rust dependency

For in-process usage, add `reddb` to your project:

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
| `encryption` | Enable encryption at rest |
| `backend-s3` | Enable S3-compatible remote storage |
| `backend-turso` | Enable Turso/libSQL backend integration |
| `backend-d1` | Enable Cloudflare D1 backend integration |

Example:

```toml
[dependencies]
reddb = { version = "0.1", features = ["query-vector", "query-graph", "encryption"] }
```

## Docker

Build the image locally:

```bash
docker build -t reddb .
```

Run HTTP:

```bash
docker run --rm -it \
  -p 8080:8080 \
  -v $(pwd)/data:/data \
  reddb red server --http --path /data/reddb.rdb --bind 0.0.0.0:8080
```

Run gRPC:

```bash
docker run --rm -it \
  -p 50051:50051 \
  -v $(pwd)/data:/data \
  reddb red server --grpc --path /data/reddb.rdb --bind 0.0.0.0:50051
```

## Linux service install

```bash
sudo ./scripts/install-systemd-service.sh \
  --binary /usr/local/bin/red \
  --grpc \
  --path /var/lib/reddb/data.rdb \
  --bind 0.0.0.0:50051
```

## Next step

After installation, go to [Connect](/getting-started/connect.md) to choose HTTP, gRPC, CLI, or embedded mode.
