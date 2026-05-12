/**
 * postinstall.js — download the matching `red` binary from GitHub Releases.
 *
 * Behavior:
 *   - Resolves `red-<platform>-<arch>` from `process.platform` + `process.arch`
 *     via the vendored asset fetcher.
 *   - Targets the GitHub release matching this package's version.
 *   - Drops the binary at `<package>/bin/red[.exe]` and chmods +x on Unix.
 *   - On any failure, prints a warning to stderr but exits 0 — `npm install`
 *     never breaks because of this script. The user gets a clear error
 *     later when they call `connect()` if the binary isn't present.
 *
 * Override hooks (env vars):
 *   REDDB_SKIP_POSTINSTALL=1      do nothing
 *   REDDB_POSTINSTALL_VERSION=…   pull a different release tag
 *   REDDB_POSTINSTALL_REPO=…      pull from a fork (default: reddb-io/reddb)
 */

import { createRequire } from 'node:module'
import { fileURLToPath } from 'node:url'
import { dirname, join, sep } from 'node:path'
import { existsSync, mkdirSync, writeFileSync, chmodSync } from 'node:fs'

import { fetchReleaseAsset } from './src/internal/asset-fetcher/index.js'

const HERE = dirname(fileURLToPath(import.meta.url))
const require = createRequire(import.meta.url)
const pkg = require('./package.json')

const DEFAULT_REPO = 'reddb-io/reddb'

if (process.env.REDDB_SKIP_POSTINSTALL === '1') {
  process.stdout.write('reddb: postinstall skipped (REDDB_SKIP_POSTINSTALL=1)\n')
  process.exit(0)
}

// Workspace-local install detection. When this file's resolved path is not
// inside a `node_modules` tree we're running from a fresh checkout of the
// reddb monorepo (or a fork), where `package.json` may already carry a
// version whose GitHub Release has not been built yet — and where the
// developer typically builds `red` with `cargo build` anyway. Skip the
// download with a clear pointer; release flows still work because the npm
// tarball, when installed, lives under `node_modules/`.
if (!HERE.includes(`${sep}node_modules${sep}`)) {
  process.stdout.write(
    'reddb: skipping postinstall — running from a workspace checkout (no node_modules/).\n' +
    '       If you need the binary, either:\n' +
    '         - cargo build --release --bin red  &&  export REDDB_BIN="$PWD/target/release/red"\n' +
    '         - or:  curl -fsSL https://raw.githubusercontent.com/reddb-io/reddb/main/install.sh | bash\n' +
    '       Set REDDB_SKIP_POSTINSTALL=1 to silence this message.\n',
  )
  process.exit(0)
}

main().catch((err) => {
  process.stderr.write(formatFailure(err))
  process.exit(0)
})

function formatFailure(err) {
  const repo = process.env.REDDB_POSTINSTALL_REPO || DEFAULT_REPO
  if (err && err.code === 'UNSUPPORTED_PLATFORM') {
    return (
      `reddb: no prebuilt red binary for ${process.platform}/${process.arch}.\n` +
      `       Options:\n` +
      `         - build from source: https://github.com/${repo}#build-from-source\n` +
      `         - or set REDDB_BIN=/path/to/red to point at one you compiled.\n`
    )
  }
  if (err && err.code === 'ASSET_NOT_FOUND') {
    return (
      `reddb: release asset not found at ${err.url}\n` +
      `       Common cause: the GitHub Release for this SDK version has not\n` +
      `       been published yet (or your platform's binary was not produced\n` +
      `       for that release). To unblock without reinstalling:\n` +
      `         1. Install the latest stable red via the official installer\n` +
      `            and point the SDK at it:\n` +
      `              curl -fsSL https://raw.githubusercontent.com/${repo}/main/install.sh | bash\n` +
      `              export REDDB_BIN="$(command -v red)"\n` +
      `         2. Or pull a specific tag explicitly and re-run postinstall:\n` +
      `              REDDB_POSTINSTALL_VERSION=v1.0.5 npm rebuild @reddb-io/sdk\n` +
      `         3. Or skip the download entirely and provide the binary yourself:\n` +
      `              REDDB_SKIP_POSTINSTALL=1 (re-install), then export REDDB_BIN=…\n` +
      `       Releases: https://github.com/${repo}/releases\n`
    )
  }
  return (
    `reddb: postinstall could not download the binary (${err && err.message}).\n` +
    `       The package itself still installs. To use the driver:\n` +
    `         - run the installer: curl -fsSL https://raw.githubusercontent.com/${repo}/main/install.sh | bash\n` +
    `           then: export REDDB_BIN="$(command -v red)"\n` +
    `         - or download manually from https://github.com/${repo}/releases\n` +
    `         - or set REDDB_BIN=/path/to/red.\n`
  )
}

async function main() {
  const repo = process.env.REDDB_POSTINSTALL_REPO || DEFAULT_REPO
  const tag = process.env.REDDB_POSTINSTALL_VERSION
    ? normalizeTag(process.env.REDDB_POSTINSTALL_VERSION)
    : `v${pkg.version}`

  const binDir = join(HERE, 'bin')
  const binaryPath = join(binDir, defaultBinaryName())

  if (existsSync(binaryPath)) {
    process.stdout.write(`reddb: binary already present at ${binaryPath}\n`)
    return
  }

  process.stdout.write(
    `reddb: downloading red ${tag} for ${process.platform}/${process.arch} from ${repo}\n`,
  )
  const body = await fetchReleaseAsset({
    repo,
    tag,
    platform: process.platform,
    arch: process.arch,
    binName: 'red',
  })
  mkdirSync(binDir, { recursive: true })
  writeFileSync(binaryPath, body)
  if (process.platform !== 'win32') {
    chmodSync(binaryPath, 0o755)
  }
  process.stdout.write(`reddb: installed binary at ${binaryPath}\n`)
}

function defaultBinaryName() {
  return process.platform === 'win32' ? 'red.exe' : 'red'
}

function normalizeTag(value) {
  const v = String(value).trim()
  return v.startsWith('v') ? v : `v${v}`
}
