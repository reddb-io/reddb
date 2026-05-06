/**
 * @reddb-io/internal-bin-resolver — runtime lookup for a pinned binary.
 *
 * Precedence:
 *   1. `process.env[envVar]` if set and non-empty → returned verbatim.
 *      No existence probe: the env var is the user's "I know what I'm
 *      doing" override.
 *   2. `<packageRoot>/bin/<name>` if it exists.
 *   3. Otherwise throw an actionable error naming the env var, the
 *      expected local path, and a one-line `pnpm install` hint.
 *
 * `PATH` is deliberately never consulted. SDK/client wire formats are
 * version-coupled to the binary; silent fallback to a stale `PATH`
 * binary fails as misframed RPC, not "command not found". See ADR 0006.
 */

import { existsSync } from 'node:fs'
import { join } from 'node:path'

/**
 * @param {{ name: string, packageRoot: string, envVar: string }} opts
 * @returns {string} absolute path to the binary
 * @throws {Error} when neither env override nor local binary is usable
 */
export function resolveBin(opts) {
  if (!opts || typeof opts !== 'object') {
    throw new TypeError('resolveBin: options object required')
  }
  const { name, packageRoot, envVar } = opts
  if (typeof name !== 'string' || name === '') {
    throw new TypeError('resolveBin: `name` must be a non-empty string')
  }
  if (typeof packageRoot !== 'string' || packageRoot === '') {
    throw new TypeError('resolveBin: `packageRoot` must be a non-empty string')
  }
  if (typeof envVar !== 'string' || envVar === '') {
    throw new TypeError('resolveBin: `envVar` must be a non-empty string')
  }

  const override = process.env?.[envVar]
  if (typeof override === 'string' && override !== '') {
    return override
  }

  const local = join(packageRoot, 'bin', name)
  if (existsSync(local)) {
    return local
  }

  throw new Error(
    `reddb: binary "${name}" not found.\n` +
      `  expected at: ${local}\n` +
      `  override:    set ${envVar}=/path/to/${name}\n` +
      `  fix:         re-run \`pnpm install\` (the postinstall script downloads it),\n` +
      `               or check the postinstall log for a download error.`,
  )
}
