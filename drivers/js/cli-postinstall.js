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
import { dirname, join } from 'node:path'
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

main().catch((err) => {
  process.stderr.write(
    `reddb-cli: postinstall could not download the binary (${err.message}).\n` +
      `          The package will still install. To use the CLI you can:\n` +
      `            - set REDDB_BIN=/path/to/red\n` +
      `            - or install the binary manually from https://github.com/${DEFAULT_REPO}/releases\n`,
  )
  process.exit(0)
})

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
