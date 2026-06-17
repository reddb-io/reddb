# Installation

RedDB ships as a single executable called `red`.

That executable is the **`red` binary**, not the same thing as:

- the embedded Rust mode
- the wire binary protocol
- the binary `.rdb` file format

In JavaScript and TypeScript, use the `@reddb-io/sdk` driver package in application code and `@reddb-io/cli` only as the npm CLI launcher.

If you want the terminology first, read [Modes and Transports](/getting-started/modes-and-transports.md).

## Install from GitHub Releases

### Recommended: installer script

The installer resolves the correct GitHub Release asset for your platform:

```bash
curl -fsSL https://raw.githubusercontent.com/reddb-io/reddb/main/install.sh | bash
```

Install a specific version:

```bash
curl -fsSL https://raw.githubusercontent.com/reddb-io/reddb/main/install.sh | bash -s -- --version v0.1.2
```

Install the prerelease channel:

```bash
curl -fsSL https://raw.githubusercontent.com/reddb-io/reddb/main/install.sh | bash -s -- --channel next
```

Change the install location:

```bash
curl -fsSL https://raw.githubusercontent.com/reddb-io/reddb/main/install.sh | bash -s -- --install-dir "$HOME/.local/bin"
```

Verify:

```bash
red version
```

### Manual release download

If you want to manage the binary yourself, download the asset for your OS and architecture from:

`https://github.com/reddb-io/reddb/releases`

Then place `red` somewhere in your `PATH`:

```bash
chmod +x ./red
sudo install -m 0755 ./red /usr/local/bin/red
red version
```

## Install with `npx`

The `@reddb-io/cli` npm package installs the real `red` binary and forwards CLI arguments directly to it.

Run a command through `npx`:

```bash
npx @reddb-io/cli@latest version
```

Start an HTTP server through `npx`:

```bash
npx @reddb-io/cli@latest server --http --path ./data/reddb.rdb --bind 127.0.0.1:8080
```

If you use `pnpm`:

```bash
pnpm dlx @reddb-io/cli version
```

## Install the JavaScript / TypeScript driver

Use the `@reddb-io/sdk` package in app code:

```bash
pnpm add @reddb-io/sdk
```

```ts
import { connect } from '@reddb-io/sdk'

const db = await connect('memory://')
const result = await db.query('SELECT * FROM users LIMIT 10')
await db.close()
```

## Troubleshooting: `npm install @reddb-io/sdk` printed a warning

The npm packages `@reddb-io/sdk`, `@reddb-io/cli`, and `@reddb-io/client` ship a `postinstall` hook that downloads the matching `red` (or `red_client`) binary from GitHub Releases. The hook is **soft-fail** — if the download can't complete, the package still installs and exits 0, but the driver can't actually run until you provide a binary.

You may see one of these warnings:

- **`release asset not found (HTTP 404)`** — usually means the GitHub Release for that SDK version has not been published yet, or your platform has no prebuilt binary in that release.
- **`no prebuilt red binary for <platform>/<arch>`** — your platform/arch combination is not (yet) produced by the release pipeline. macOS Intel (`darwin/x64`) was added recently and is only present from `v1.0.6` onward.

### Three ways to unblock

1. **Install the latest stable `red` via the official installer and point the SDK at it.** The SDK consults `REDDB_BIN` before anything else, so this works regardless of which release contains your platform's asset:

   ```bash
   curl -fsSL https://raw.githubusercontent.com/reddb-io/reddb/main/install.sh | bash
   export REDDB_BIN="$(command -v red)"
   ```

2. **Pin the postinstall to a release tag you know exists** and re-run the hook:

   ```bash
   REDDB_POSTINSTALL_VERSION=v1.0.5 pnpm rebuild @reddb-io/sdk
   # or:  REDDB_POSTINSTALL_VERSION=v1.0.5 npm rebuild @reddb-io/sdk
   ```

   Check available tags at <https://github.com/reddb-io/reddb/releases>.

3. **Skip the download entirely** if you'll bring your own binary:

   ```bash
   REDDB_SKIP_POSTINSTALL=1 pnpm add @reddb-io/sdk
   export REDDB_BIN=/path/to/red
   ```

### Postinstall env-var reference

| Variable                    | Effect                                                        |
|-----------------------------|---------------------------------------------------------------|
| `REDDB_BIN`                 | Runtime override consulted by `@reddb-io/sdk` and `@reddb-io/cli` before falling back to the bundled binary. Also tells `cli-postinstall` to skip downloading. |
| `REDDB_CLIENT_BIN`          | Same idea for `@reddb-io/client`'s `red_client` helper.       |
| `REDDB_SKIP_POSTINSTALL=1`  | Don't try to download anything during `npm install`.          |
| `REDDB_POSTINSTALL_VERSION` | Pull a specific release tag instead of `v${pkg.version}`.     |
| `REDDB_POSTINSTALL_REPO`    | Pull from a fork (defaults to `reddb-io/reddb`).              |

## Build from source

RedDB requires Rust and `protoc`.

```bash
git clone https://github.com/reddb-io/reddb.git
cd reddb
cargo build --release --bin red
./target/release/red version
```

Install the built binary:

```bash
sudo install -m 0755 target/release/red /usr/local/bin/red
```

## Use as an embedded Rust dependency

For in-process usage, add `reddb-io` to your project:

```toml
[dependencies]
reddb-io = "1.0"
```

The crate publishes on crates.io as `reddb-io`; the in-code import path stays `use reddb::…`.

Optional feature flags:

| Feature | Description |
|:--------|:------------|
| `otel` | Enable OpenTelemetry scaffolding |
| `backend-s3` | Enable S3-compatible remote storage |
| `backend-turso` | Enable Turso/libSQL backend integration |
| `backend-d1` | Enable Cloudflare D1 backend integration |

Example:

```toml
[dependencies]
reddb-io = { version = "1.0", features = ["backend-s3", "otel"] }
```

## Docker

Prebuilt images are published to GHCR. If the package is private in your
environment, authenticate before pulling:

```bash
echo "$GITHUB_TOKEN" | docker login ghcr.io -u "$GITHUB_USER" --password-stdin
docker pull ghcr.io/reddb-io/reddb:latest
```

If you do not have GHCR access, build locally from the checkout instead.

Build the image locally:

```bash
docker build -t reddb .
```

Run HTTP:

```bash
docker run --rm -it \
  -p 55880:8080 \
  -v $(pwd)/data:/data \
  reddb red server --http --path /data/reddb.rdb --bind 0.0.0.0:8080
```

Run gRPC:

```bash
docker run --rm -it \
  -p 55551:50051 \
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

After installation, go to [Modes and Transports](/getting-started/modes-and-transports.md) to choose between embedded mode, the standalone `red` process, router, HTTP, gRPC, wire, and stdio bridge usage.
