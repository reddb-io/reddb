/**
 * postinstall.js — download the matching `red` binary from GitHub Releases.
 *
 * Behavior:
 *   - Resolves `red-<platform>-<arch>` from `process.platform` + `process.arch`
 *     via the vendored asset fetcher.
 *   - Targets the GitHub release matching this package's version.
 *   - Drops the binary at `<package>/bin/red[.exe]` and chmods +x on Unix.
 *   - Intentional skip paths (workspace checkout, REDDB_SKIP_POSTINSTALL=1)
 *     exit 0 quietly. Any unintentional failure (no network, 404, asset
 *     fetcher error, unsupported platform) prints an actionable multi-line
 *     message to stderr and exits 1 so `npm install` fails loud — never
 *     ships a package with an empty bin/ that explodes later at connect().
 *
 * Override hooks (env vars):
 *   REDDB_SKIP_POSTINSTALL=1      do nothing, exit 0 (caller will provide
 *                                 the binary via REDDB_BIN or a vendored
 *                                 copy at bin/red[.exe])
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
  process.exit(1)
})

function formatFailure(err) {
  const repo = process.env.REDDB_POSTINSTALL_REPO || DEFAULT_REPO
  const escapeHatches =
    `       Escape hatches (any one of these will unblock the install):\n` +
    `         - Skip the download and provide the binary yourself:\n` +
    `             REDDB_SKIP_POSTINSTALL=1 npm install\n` +
    `             then at runtime:  export REDDB_BIN=/path/to/red\n` +
    `         - Or install red via the official installer and point at it:\n` +
    `             curl -fsSL https://raw.githubusercontent.com/${repo}/main/install.sh | bash\n` +
    `             export REDDB_BIN="$(command -v red)"\n` +
    `         - Or build from a workspace checkout of ${repo}:\n` +
    `             cargo build --release --bin red\n` +
    `             export REDDB_BIN="$PWD/target/release/red"\n` +
    `         - Or download the binary manually from\n` +
    `             https://github.com/${repo}/releases\n` +
    `           and place it at <package>/bin/red[.exe]\n`

  if (err && err.code === 'UNSUPPORTED_PLATFORM') {
    return (
      `reddb: no prebuilt red binary for ${process.platform}/${process.arch}.\n` +
      `       Building from source is required for this platform:\n` +
      `         https://github.com/${repo}#build-from-source\n` +
      escapeHatches
    )
  }
  if (err && err.code === 'ASSET_NOT_FOUND') {
    return (
      `reddb: release asset not found at ${err.url}\n` +
      `       Common cause: the GitHub Release for this SDK version has not\n` +
      `       been published yet (or your platform's binary was not produced\n` +
      `       for that release). You can also pull a specific tag explicitly:\n` +
      `         REDDB_POSTINSTALL_VERSION=v1.0.5 npm rebuild @reddb-io/sdk\n` +
      escapeHatches
    )
  }
  return (
    `reddb: postinstall could not download the red binary (${(err && err.message) || err}).\n` +
    `       Install failed — the SDK will not work without a binary at\n` +
    `       <package>/bin/red[.exe] or pointed at by REDDB_BIN.\n` +
    escapeHatches
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
