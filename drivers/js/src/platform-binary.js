/**
 * Locate the `red` binary shipped as a per-platform npm package.
 *
 * `@reddb-io/cli` and `@reddb-io/sdk` declare every `@reddb-io/red-<key>`
 * package as an `optionalDependency`; the package manager installs only the
 * one matching the host's `os` / `cpu` / `libc`, so there is no install
 * script and no download. `<key>` is `<process.platform>-<process.arch>`,
 * plus `-musl` for the static Linux builds. `scripts/npm-platform-packages.mjs`
 * owns the publish side; a contract test keeps the two key tables identical.
 */

import { existsSync } from 'node:fs'
import { createRequire } from 'node:module'
import { dirname, join } from 'node:path'

const require = createRequire(import.meta.url)

/** `glibc` | `musl` on Linux, `null` elsewhere. */
export function detectLibc(platform = process.platform) {
  if (platform !== 'linux') return null
  const header = process.report?.getReport?.()?.header
  return header?.glibcVersionRuntime ? 'glibc' : 'musl'
}

/**
 * Package names to try, best match first. On Linux the other libc variant is
 * a fallback: package managers that ignore `libc` install both, and the
 * static (musl) binary runs on glibc hosts too.
 */
export function platformPackageNames({
  platform = process.platform,
  arch = process.arch,
  libc = detectLibc(platform),
} = {}) {
  const base = `@reddb-io/red-${platform}-${arch}`
  if (platform !== 'linux') return [base]
  return libc === 'musl' ? [`${base}-musl`, base] : [base, `${base}-musl`]
}

/**
 * @returns {string | null} absolute path to the binary, or null when no
 *   platform package for this host is installed.
 */
export function resolvePlatformBinary({
  platform = process.platform,
  arch = process.arch,
  libc = detectLibc(platform),
  resolve = (id) => require.resolve(id),
} = {}) {
  const binName = platform === 'win32' ? 'red.exe' : 'red'
  for (const name of platformPackageNames({ platform, arch, libc })) {
    let manifest
    try {
      manifest = resolve(`${name}/package.json`)
    } catch {
      continue
    }
    const binary = join(dirname(manifest), 'bin', binName)
    if (existsSync(binary)) return binary
  }
  return null
}
