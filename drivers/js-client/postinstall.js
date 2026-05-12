/**
 * postinstall.js — download the matching `red_client` binary from
 * GitHub Releases.
 *
 * Behaviour:
 *   - Resolves `red_client-<platform>-<arch>` from process.platform +
 *     process.arch via the vendored asset fetcher.
 *   - Targets the GitHub release matching this package's version.
 *   - Drops the binary at `<package>/bin/red_client[.exe]` and chmods
 *     +x on Unix.
 *   - On any failure, prints a warning to stderr but exits 0 — npm
 *     install never breaks because of this script. The user gets a
 *     clear error later when they call `connect()` if the binary is
 *     missing AND the runtime needs to spawn it. (The remote-only
 *     transports in @reddb-io/client don't actually need the binary
 *     for `connect()` itself; the binary is provided as a CLI helper
 *     for ad-hoc REPL / one-shot SQL.)
 *
 * Override hooks (env vars):
 *   REDDB_SKIP_POSTINSTALL=1      do nothing
 *   REDDB_POSTINSTALL_VERSION=…   pull a different release tag
 *   REDDB_POSTINSTALL_REPO=…      pull from a fork (default: reddb-io/reddb)
 *   REDDB_CLIENT_BIN=/path        runtime override consulted by callers
 *                                 that spawn the binary; postinstall
 *                                 still downloads to the package dir
 *                                 unless REDDB_SKIP_POSTINSTALL=1.
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
const BIN_NAME = 'red_client'

if (process.env.REDDB_SKIP_POSTINSTALL === '1') {
  process.stdout.write('@reddb-io/client: postinstall skipped (REDDB_SKIP_POSTINSTALL=1)\n')
  process.exit(0)
}

// Workspace-local install detection — see drivers/js/postinstall.js for
// the matching guard and rationale. connect() in this package doesn't need
// the binary anyway; the helper CLI is the only consumer.
if (!HERE.includes(`${sep}node_modules${sep}`)) {
  process.stdout.write(
    '@reddb-io/client: skipping postinstall — running from a workspace checkout (no node_modules/).\n' +
    '       connect() works without the binary. If you also want the red_client CLI:\n' +
    '         cargo build --release --bin red_client -p reddb-io-client --no-default-features\n' +
    '         export REDDB_CLIENT_BIN="$PWD/target/release/red_client"\n' +
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
      `@reddb-io/client: no prebuilt red_client binary for ${process.platform}/${process.arch}.\n`
      + `       Note: connect() itself does not need the binary — it is only used as a CLI helper.\n`
      + `       Options:\n`
      + `         - build from source: https://github.com/${repo}#build-from-source\n`
      + `         - or set REDDB_CLIENT_BIN=/path/to/red_client.\n`
    )
  }
  if (err && err.code === 'ASSET_NOT_FOUND') {
    return (
      `@reddb-io/client: release asset not found at ${err.url}\n`
      + `       The GitHub Release for this client version likely has not been\n`
      + `       published yet. The driver still installs and connect() works\n`
      + `       without the binary; this only affects the bundled CLI helper.\n`
      + `       To unblock:\n`
      + `         1. Pull a specific tag and rebuild:\n`
      + `              REDDB_POSTINSTALL_VERSION=v1.0.5 npm rebuild @reddb-io/client\n`
      + `         2. Or download red_client manually and export REDDB_CLIENT_BIN.\n`
      + `       Releases: https://github.com/${repo}/releases\n`
    )
  }
  return (
    `@reddb-io/client: postinstall could not download red_client (${err && err.message}).\n`
    + `       The package itself still installs (connect() does not need the binary).\n`
    + `       To get the bundled CLI working:\n`
    + `         - set REDDB_CLIENT_BIN=/path/to/red_client\n`
    + `         - or install it manually from https://github.com/${repo}/releases\n`
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
    process.stdout.write(`@reddb-io/client: binary already present at ${binaryPath}\n`)
    return
  }

  process.stdout.write(
    `@reddb-io/client: downloading ${BIN_NAME} ${tag} for ${process.platform}/${process.arch} from ${repo}\n`,
  )
  const body = await fetchReleaseAsset({
    repo,
    tag,
    platform: process.platform,
    arch: process.arch,
    binName: BIN_NAME,
  })
  mkdirSync(binDir, { recursive: true })
  writeFileSync(binaryPath, body)
  if (process.platform !== 'win32') {
    chmodSync(binaryPath, 0o755)
  }
  process.stdout.write(`@reddb-io/client: installed binary at ${binaryPath}\n`)
}

function defaultBinaryName() {
  return process.platform === 'win32' ? `${BIN_NAME}.exe` : BIN_NAME
}

function normalizeTag(value) {
  const v = String(value).trim()
  return v.startsWith('v') ? v : `v${v}`
}
