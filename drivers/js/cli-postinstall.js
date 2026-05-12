/**
 * cli-postinstall.js — install/upgrade/skip for `@reddb-io/cli`.
 *
 * Asymmetry vs SDK postinstall (`drivers/js/postinstall.js`):
 *   - SDK always downloads — wire format is version-coupled to the driver.
 *   - CLI compares this package's version against `red --version` on PATH:
 *       absent  → install
 *       older   → upgrade (overwrite + one-line log)
 *       equal   → skip
 *       newer   → skip ("PATH binary is newer; leaving in place")
 *       garbage → install (warn, treat as absent)
 *
 * See ADR 0007 for the rationale.
 *
 * Override hooks (env vars):
 *   REDDB_SKIP_POSTINSTALL=1      do nothing
 *   REDDB_BIN=/path/to/red        skip — caller already has a binary
 *   REDDB_POSTINSTALL_VERSION=…   pull a different release tag
 *   REDDB_POSTINSTALL_REPO=…      pull from a fork (default: reddb-io/reddb)
 */

import { createRequire } from 'node:module'
import { fileURLToPath } from 'node:url'
import { dirname, join, sep } from 'node:path'
import { existsSync, mkdirSync, writeFileSync, chmodSync } from 'node:fs'
import { execSync } from 'node:child_process'

import { fetchReleaseAsset } from './src/internal/asset-fetcher/index.js'
import { compareInstalled } from './src/internal/version-compare/index.js'

const HERE = dirname(fileURLToPath(import.meta.url))
const require = createRequire(import.meta.url)
// CLI manifest lives at the repo root (../../package.json), and that is the
// `@reddb-io/cli` package — not the SDK manifest next to this file.
const pkg = require('../../package.json')

const DEFAULT_REPO = 'reddb-io/reddb'

if (process.env.REDDB_SKIP_POSTINSTALL === '1') {
  process.stdout.write('reddb-cli: postinstall skipped (REDDB_SKIP_POSTINSTALL=1)\n')
  process.exit(0)
}

if (typeof process.env.REDDB_BIN === 'string' && process.env.REDDB_BIN !== '') {
  process.stdout.write(
    `reddb-cli: postinstall skipped (REDDB_BIN=${process.env.REDDB_BIN})\n`,
  )
  process.exit(0)
}

// Workspace-local install detection — see the matching block in
// drivers/js/postinstall.js for the rationale.
if (!HERE.includes(`${sep}node_modules${sep}`)) {
  process.stdout.write(
    'reddb-cli: skipping postinstall — running from a workspace checkout (no node_modules/).\n' +
    '          Build `red` locally if you need it:\n' +
    '            cargo build --release --bin red\n' +
    '            export REDDB_BIN="$PWD/target/release/red"\n' +
    '          Set REDDB_SKIP_POSTINSTALL=1 to silence this message.\n',
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
      `reddb-cli: no prebuilt red binary for ${process.platform}/${process.arch}.\n` +
      `          Options:\n` +
      `            - build from source: https://github.com/${repo}#build-from-source\n` +
      `            - or set REDDB_BIN=/path/to/red.\n`
    )
  }
  if (err && err.code === 'ASSET_NOT_FOUND') {
    return (
      `reddb-cli: release asset not found at ${err.url}\n` +
      `          The GitHub Release for this CLI version likely has not been\n` +
      `          published yet. To unblock:\n` +
      `            1. Use the installer (latest stable red on PATH):\n` +
      `                 curl -fsSL https://raw.githubusercontent.com/${repo}/main/install.sh | bash\n` +
      `            2. Or pull a specific tag and rebuild:\n` +
      `                 REDDB_POSTINSTALL_VERSION=v1.0.5 npm rebuild @reddb-io/cli\n` +
      `            3. Or point the launcher at an existing binary:\n` +
      `                 export REDDB_BIN=/path/to/red\n` +
      `          Releases: https://github.com/${repo}/releases\n`
    )
  }
  return (
    `reddb-cli: postinstall could not download the binary (${err && err.message}).\n` +
    `          The package itself still installs. To use the CLI:\n` +
    `            - run the installer: curl -fsSL https://raw.githubusercontent.com/${repo}/main/install.sh | bash\n` +
    `            - or download manually from https://github.com/${repo}/releases\n` +
    `            - or set REDDB_BIN=/path/to/red.\n`
  )
}

async function main() {
  const verdict = compareInstalled({
    packageVersion: pkg.version,
    exec: () => execSync('red --version', { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }),
  })

  if (verdict.action === 'skip') {
    process.stdout.write(`reddb-cli: ${verdict.reason} — skipping download\n`)
    return
  }

  const repo = process.env.REDDB_POSTINSTALL_REPO || DEFAULT_REPO
  const tag = process.env.REDDB_POSTINSTALL_VERSION
    ? normalizeTag(process.env.REDDB_POSTINSTALL_VERSION)
    : `v${pkg.version}`

  const binDir = join(HERE, 'bin')
  const binaryPath = join(binDir, defaultBinaryName())

  if (verdict.action === 'install') {
    process.stdout.write(`reddb-cli: ${verdict.reason} — installing red ${tag}\n`)
  } else {
    // upgrade
    process.stdout.write(`reddb-cli: ${verdict.reason} — upgrading red to ${tag}\n`)
  }

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
  process.stdout.write(`reddb-cli: installed binary at ${binaryPath}\n`)
}

function defaultBinaryName() {
  return process.platform === 'win32' ? 'red.exe' : 'red'
}

function normalizeTag(value) {
  const v = String(value).trim()
  return v.startsWith('v') ? v : `v${v}`
}
