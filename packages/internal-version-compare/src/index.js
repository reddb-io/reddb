/**
 * @reddb-io/internal-version-compare — install/upgrade/skip verdict for
 * the CLI postinstall.
 *
 * Asymmetry vs SDK/client (per ADR 0006):
 *   - SDK/client postinstalls *always* download a pinned binary because
 *     the wire format is version-coupled to the driver.
 *   - The CLI is a launcher. Users typically already have `red` on PATH
 *     and would resent each `npm i -g @reddb-io/cli` re-downloading.
 *     We compare versions and only fetch when strictly newer.
 *
 * Verdict table:
 *   exec throws                            → action='install'  (no binary detected)
 *   exec returns unparseable output        → action='install'  (warn, fetch fresh)
 *   PATH version  <  packageVersion        → action='upgrade'
 *   PATH version  >= packageVersion        → action='skip'
 *
 * Prerelease ordering follows semver 2.0.0 §11: a normal release ranks
 * above any prerelease of the same MAJOR.MINOR.PATCH.
 */

const SEMVER_RE = /\b(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?\b/

/**
 * Extract a semver-shaped version string from arbitrary `red --version`
 * output. Accepts `reddb 0.2.9`, bare `0.2.9`, or `red 1.0.0-rc.1`.
 *
 * @param {string} text
 * @returns {string|null} normalised `MAJOR.MINOR.PATCH[-prerelease]` or null
 */
export function parseVersion(text) {
  if (typeof text !== 'string') return null
  const m = text.match(SEMVER_RE)
  if (!m) return null
  const [, maj, min, pat, pre] = m
  return pre ? `${maj}.${min}.${pat}-${pre}` : `${maj}.${min}.${pat}`
}

/**
 * semver 2.0.0 ordering. Returns -1 / 0 / 1.
 *
 * Throws TypeError if either side is not parseable — this is a
 * programmer error, not a runtime fallback path.
 */
export function compareSemver(a, b) {
  const pa = parseVersion(a)
  const pb = parseVersion(b)
  if (pa === null) throw new TypeError(`compareSemver: invalid version "${a}"`)
  if (pb === null) throw new TypeError(`compareSemver: invalid version "${b}"`)

  const [coreA, preA] = splitCore(pa)
  const [coreB, preB] = splitCore(pb)

  for (let i = 0; i < 3; i++) {
    if (coreA[i] !== coreB[i]) return coreA[i] < coreB[i] ? -1 : 1
  }

  // Cores equal — apply §11: no-prerelease ranks above any prerelease.
  if (preA === null && preB === null) return 0
  if (preA === null) return 1
  if (preB === null) return -1

  return comparePrerelease(preA, preB)
}

function splitCore(v) {
  const dash = v.indexOf('-')
  const core = dash === -1 ? v : v.slice(0, dash)
  const pre = dash === -1 ? null : v.slice(dash + 1)
  const parts = core.split('.').map((n) => Number(n))
  return [parts, pre]
}

function comparePrerelease(a, b) {
  const sa = a.split('.')
  const sb = b.split('.')
  const len = Math.min(sa.length, sb.length)
  for (let i = 0; i < len; i++) {
    const c = compareIdentifier(sa[i], sb[i])
    if (c !== 0) return c
  }
  if (sa.length === sb.length) return 0
  return sa.length < sb.length ? -1 : 1
}

function compareIdentifier(a, b) {
  const aNum = /^\d+$/.test(a)
  const bNum = /^\d+$/.test(b)
  if (aNum && bNum) {
    const na = Number(a)
    const nb = Number(b)
    return na === nb ? 0 : na < nb ? -1 : 1
  }
  if (aNum) return -1 // numeric < alphanumeric per §11
  if (bNum) return 1
  return a === b ? 0 : a < b ? -1 : 1
}

/**
 * Decide what the CLI postinstall should do.
 *
 * @param {{ packageVersion: string, exec: () => string }} opts
 *   `exec` is called with no arguments and is expected to return the
 *   stdout of `red --version` (sync). It may throw — that signals "no
 *   PATH binary detected" and routes to action='install'.
 * @returns {{ action: 'install'|'upgrade'|'skip', reason: string }}
 */
export function compareInstalled(opts) {
  if (!opts || typeof opts !== 'object') {
    throw new TypeError('compareInstalled: options object required')
  }
  const { packageVersion, exec } = opts
  if (typeof packageVersion !== 'string' || packageVersion === '') {
    throw new TypeError('compareInstalled: `packageVersion` must be a non-empty string')
  }
  if (typeof exec !== 'function') {
    throw new TypeError('compareInstalled: `exec` must be a function')
  }
  if (parseVersion(packageVersion) === null) {
    throw new TypeError(`compareInstalled: \`packageVersion\` "${packageVersion}" is not parseable semver`)
  }

  let raw
  try {
    raw = exec()
  } catch (err) {
    return {
      action: 'install',
      reason: `no PATH \`red\` binary detected (${err.message || 'exec failed'})`,
    }
  }

  const installed = parseVersion(typeof raw === 'string' ? raw : '')
  if (installed === null) {
    return {
      action: 'install',
      reason: `unparseable PATH \`red --version\` output (${truncate(String(raw))})`,
    }
  }

  const cmp = compareSemver(installed, packageVersion)
  if (cmp < 0) {
    return {
      action: 'upgrade',
      reason: `PATH \`red\` ${installed} is older than package ${packageVersion}`,
    }
  }
  if (cmp > 0) {
    return {
      action: 'skip',
      reason: `PATH \`red\` ${installed} is newer than package ${packageVersion}`,
    }
  }
  return {
    action: 'skip',
    reason: `PATH \`red\` already at ${installed}`,
  }
}

function truncate(s) {
  const oneLine = s.replace(/\s+/g, ' ').trim()
  return oneLine.length > 80 ? `${oneLine.slice(0, 77)}...` : oneLine
}
