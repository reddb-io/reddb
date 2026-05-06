/**
 * postinstall.js — download the matching `red_client` binary from
 * GitHub Releases.
 *
 * Behaviour:
 *   - Resolves `red_client-<platform>-<arch>` from process.platform +
 *     process.arch via @reddb-io/internal-asset-fetcher.
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
import { dirname, join } from 'node:path'
import { existsSync, mkdirSync, writeFileSync, chmodSync } from 'node:fs'

import { fetchReleaseAsset } from '@reddb-io/internal-asset-fetcher'

const HERE = dirname(fileURLToPath(import.meta.url))
const require = createRequire(import.meta.url)
const pkg = require('./package.json')

const DEFAULT_REPO = 'reddb-io/reddb'
const BIN_NAME = 'red_client'

if (process.env.REDDB_SKIP_POSTINSTALL === '1') {
  process.stdout.write('@reddb-io/client: postinstall skipped (REDDB_SKIP_POSTINSTALL=1)\n')
  process.exit(0)
}

main().catch((err) => {
  process.stderr.write(
    `@reddb-io/client: postinstall could not download red_client (${err.message}).\n`
      + `       The package will still install. To use the binary you can:\n`
      + `         - set REDDB_CLIENT_BIN=/path/to/red_client\n`
      + `         - or install it manually from https://github.com/${DEFAULT_REPO}/releases\n`,
  )
  process.exit(0)
})

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
