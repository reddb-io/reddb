/**
 * Locate (or download) the `red` binary for the `@reddb-io/mcp` launcher.
 *
 * Resolution precedence (reuses the vendored bin-resolver — env > local,
 * PATH is never consulted, per ADR 0006):
 *   1. `REDDB_BIN` env var → returned verbatim.
 *   2. `<package>/bin/red[.exe]` if already present (e.g. a previous npx run
 *      cached it, or a workspace checkout vendored it).
 *   3. Otherwise download the matching GitHub Release asset (reusing the
 *      vendored asset-fetcher — no new download logic) into `<package>/bin/`
 *      and return that path.
 *
 * The download tag tracks this package's own version (`v<pkg.version>`),
 * which the release flow keeps locked to the engine version. Because
 * `npx -y @reddb-io/mcp` always pulls the latest published launcher, this
 * yields the latest released engine with zero local install.
 */

import { existsSync, mkdirSync, writeFileSync, chmodSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, resolve, join } from 'node:path'

import { resolveBin } from './internal/bin-resolver/index.js'
import { fetchReleaseAsset } from './internal/asset-fetcher/index.js'

const HERE = dirname(fileURLToPath(import.meta.url))
const PACKAGE_ROOT = resolve(HERE, '..')

export const DEFAULT_REPO = 'reddb-io/reddb'

/** Platform-specific base name of the engine binary. */
export function binaryName(platform = process.platform) {
  return platform === 'win32' ? 'red.exe' : 'red'
}

/**
 * Non-throwing probe reusing the vendored bin-resolver: returns an
 * absolute path when `REDDB_BIN` is set or a local binary exists, else
 * `null` (so the caller can decide to download).
 *
 * @param {{ packageRoot?: string, platform?: string }} [opts]
 * @returns {string|null}
 */
export function tryResolveBinary({ packageRoot = PACKAGE_ROOT, platform = process.platform } = {}) {
  try {
    return resolveBin({ name: binaryName(platform), packageRoot, envVar: 'REDDB_BIN' })
  } catch {
    return null
  }
}

/**
 * Resolve the engine binary, downloading it from GitHub Releases when it is
 * not already available. Returns the absolute path to a runnable binary.
 *
 * @param {{
 *   version: string,
 *   packageRoot?: string,
 *   platform?: string,
 *   arch?: string,
 *   repo?: string,
 *   fetchAsset?: typeof fetchReleaseAsset,
 * }} opts
 * @returns {Promise<string>}
 */
export async function ensureBinary({
  version,
  packageRoot = PACKAGE_ROOT,
  platform = process.platform,
  arch = process.arch,
  repo = process.env.REDDB_MCP_REPO || DEFAULT_REPO,
  fetchAsset = fetchReleaseAsset,
} = {}) {
  if (typeof version !== 'string' || version === '') {
    throw new TypeError('ensureBinary: `version` must be a non-empty string')
  }

  const existing = tryResolveBinary({ packageRoot, platform })
  if (existing) {
    return existing
  }

  const tag = normalizeTag(process.env.REDDB_MCP_VERSION || version)
  const body = await fetchAsset({ repo, tag, platform, arch, binName: 'red' })

  const binDir = join(packageRoot, 'bin')
  mkdirSync(binDir, { recursive: true })
  const dest = join(binDir, binaryName(platform))
  writeFileSync(dest, body)
  if (platform !== 'win32') {
    chmodSync(dest, 0o755)
  }
  return dest
}

function normalizeTag(value) {
  const v = String(value).trim()
  return v.startsWith('v') ? v : `v${v}`
}
