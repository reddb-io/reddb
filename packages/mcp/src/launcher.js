#!/usr/bin/env node
/**
 * @reddb-io/mcp — zero-install npx launcher for the RedDB MCP surface.
 *
 *   npx -y @reddb-io/mcp [args…]
 *
 * Resolves (or downloads) the matching `red` binary and execs `red mcp`
 * over stdio. No tool/knowledge logic lives here — the launcher only
 * fetches + spawns the native engine, so the MCP surface is always the
 * latest released engine's.
 */

import { createRequire } from 'node:module'

import { ensureBinary, DEFAULT_REPO } from './binary.js'
import { spawnMcp } from './spawn.js'

const require = createRequire(import.meta.url)
const pkg = require('../package.json')

ensureBinary({ version: pkg.version })
  .then((binary) => {
    const child = spawnMcp(binary, process.argv.slice(2))
    child.on('error', (err) => {
      process.stderr.write(`reddb-mcp: failed to spawn red (${err.message})\n`)
      process.exit(1)
    })
    child.on('exit', (code, signal) => {
      process.exit(signal ? 1 : (code ?? 0))
    })
  })
  .catch((err) => {
    process.stderr.write(formatFailure(err))
    process.exit(1)
  })

function formatFailure(err) {
  const repo = process.env.REDDB_MCP_REPO || DEFAULT_REPO
  const escapeHatches =
    `       Escape hatches:\n` +
    `         - Provide the binary yourself:  export REDDB_BIN=/path/to/red\n` +
    `         - Install red via the official installer:\n` +
    `             curl -fsSL https://raw.githubusercontent.com/${repo}/main/install.sh | bash\n` +
    `             export REDDB_BIN="$(command -v red)"\n` +
    `         - Download a release binary manually from\n` +
    `             https://github.com/${repo}/releases\n`

  if (err && err.code === 'UNSUPPORTED_PLATFORM') {
    return (
      `reddb-mcp: no prebuilt red binary for ${process.platform}/${process.arch}.\n` +
      `       Build from source: https://github.com/${repo}#build-from-source\n` +
      escapeHatches
    )
  }
  if (err && err.code === 'ASSET_NOT_FOUND') {
    return (
      `reddb-mcp: release asset not found${err.url ? ` at ${err.url}` : ''}.\n` +
      `       The GitHub Release for this launcher version may not be published yet,\n` +
      `       or your platform's binary was not produced for that release.\n` +
      escapeHatches
    )
  }
  return (
    `reddb-mcp: could not obtain the red binary (${(err && err.message) || err}).\n` +
    escapeHatches
  )
}
