#!/usr/bin/env node
/**
 * npm-platform-packages.mjs — ship the `red` binary as per-platform npm
 * packages (esbuild-style) instead of a `postinstall` download.
 *
 * `@reddb-io/cli` and `@reddb-io/sdk` list these packages as
 * `optionalDependencies`; npm/pnpm/yarn install only the one whose
 * `os`/`cpu`/`libc` match the host, so installing needs no script and no
 * network beyond the registry. See ADR 0007 (amendment: alternative D).
 *
 * Subcommands (run from the repo root):
 *
 *   build  --tag vX.Y.Z --version X.Y.Z --assets <dir> --out <dir> [--allow-missing]
 *     Turn release assets (`red-linux-x86_64`, ... + `SHA256SUMS`) into
 *     publishable package directories under <out>, verifying every binary
 *     against SHA256SUMS. Prints one package directory per line.
 *     Missing optional platforms (macOS, Windows) are skipped; a missing
 *     required one is an error unless --allow-missing (release candidates
 *     only build linux-x86_64).
 *     When the release also carries the in-process Node addon for a platform
 *     (`reddb-node-<suffix>.node`) it is verified the same way and shipped in
 *     the package as `reddb.node`, which `@reddb-io/sdk` loads instead of
 *     spawning `red`. The addon is optional: a platform without one still
 *     gets its package, and the SDK falls back to the `red` subprocess.
 *
 *   inject --version X.Y.Z <package.json>...
 *     Add every platform package as an exact-version `optionalDependency`.
 *     Done at publish time, not committed: the packages do not exist in the
 *     registry until the release publishes them, so declaring them in the
 *     workspace would break `pnpm-lock.yaml`.
 */

import { createHash } from 'node:crypto'
import { chmodSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

/**
 * `key` is `<process.platform>-<process.arch>[-musl]` — the same key the
 * runtime resolver (`drivers/js/src/platform-binary.js`) derives. A contract
 * test keeps the two tables identical.
 */
export const PLATFORMS = [
  { key: 'linux-x64', os: 'linux', cpu: 'x64', libc: 'glibc', asset: 'red-linux-x86_64', required: true },
  { key: 'linux-x64-musl', os: 'linux', cpu: 'x64', libc: 'musl', asset: 'red-linux-x86_64-static', required: true },
  { key: 'linux-arm64', os: 'linux', cpu: 'arm64', libc: 'glibc', asset: 'red-linux-aarch64', required: true },
  { key: 'linux-arm64-musl', os: 'linux', cpu: 'arm64', libc: 'musl', asset: 'red-linux-aarch64-static', required: true },
  { key: 'linux-arm', os: 'linux', cpu: 'arm', libc: 'glibc', asset: 'red-linux-armv7', required: true },
  { key: 'darwin-x64', os: 'darwin', cpu: 'x64', asset: 'red-macos-x86_64', required: false },
  { key: 'darwin-arm64', os: 'darwin', cpu: 'arm64', asset: 'red-macos-aarch64', required: false },
  { key: 'win32-x64', os: 'win32', cpu: 'x64', asset: 'red-windows-x86_64.exe', required: false },
]

export const packageName = (p) => `@reddb-io/red-${p.key}`

/** Release asset name of the Node addon built for platform `p`. */
export const addonAssetName = (p) => `${p.asset.replace(/^red-/, 'reddb-node-').replace(/\.exe$/, '')}.node`

export function parseSha256Sums(text) {
  const sums = new Map()
  for (const line of text.split('\n')) {
    const m = line.match(/^([0-9a-fA-F]{64})\s+\*?(\S+)\s*$/)
    if (m) sums.set(m[2], m[1].toLowerCase())
  }
  return sums
}

export function platformManifest(p, version, { addon = false } = {}) {
  const manifest = {
    name: packageName(p),
    version,
    description: `The RedDB \`red\` binary for ${p.os} ${p.cpu}${p.libc ? ` (${p.libc})` : ''}. Installed automatically by @reddb-io/cli and @reddb-io/sdk; do not depend on it directly.`,
    os: [p.os],
    cpu: [p.cpu],
    ...(p.libc ? { libc: [p.libc] } : {}),
    files: addon ? ['bin/', 'reddb.node'] : ['bin/'],
    license: 'MIT',
    homepage: 'https://github.com/reddb-io/reddb',
    repository: { type: 'git', url: 'git+https://github.com/reddb-io/reddb.git' },
    publishConfig: { access: 'public' },
  }
  return manifest
}

export function buildPackages({ version, assetsDir, outDir, allowMissing = false }) {
  const sumsPath = join(assetsDir, 'SHA256SUMS')
  if (!existsSync(sumsPath)) throw new Error(`SHA256SUMS not found in ${assetsDir}`)
  const sums = parseSha256Sums(readFileSync(sumsPath, 'utf8'))

  const built = []
  for (const p of PLATFORMS) {
    const assetPath = join(assetsDir, p.asset)
    if (!existsSync(assetPath)) {
      if (p.required && !allowMissing) throw new Error(`required asset missing: ${p.asset}`)
      continue
    }
    const expected = sums.get(p.asset)
    if (!expected) throw new Error(`no SHA256SUMS entry for ${p.asset}; refusing to package an unverified binary`)
    const body = readFileSync(assetPath)
    const actual = createHash('sha256').update(body).digest('hex')
    if (actual !== expected) throw new Error(`sha256 mismatch for ${p.asset}: expected ${expected}, got ${actual}`)

    const dir = join(outDir, `red-${p.key}`)
    mkdirSync(join(dir, 'bin'), { recursive: true })
    const binary = join(dir, 'bin', p.os === 'win32' ? 'red.exe' : 'red')
    writeFileSync(binary, body)
    chmodSync(binary, 0o755)

    let addon = false
    const addonName = addonAssetName(p)
    const addonPath = join(assetsDir, addonName)
    if (existsSync(addonPath)) {
      const addonBody = readFileSync(addonPath)
      const addonExpected = sums.get(addonName)
      if (!addonExpected) throw new Error(`no SHA256SUMS entry for ${addonName}; refusing to package an unverified addon`)
      const addonActual = createHash('sha256').update(addonBody).digest('hex')
      if (addonActual !== addonExpected) throw new Error(`sha256 mismatch for ${addonName}: expected ${addonExpected}, got ${addonActual}`)
      writeFileSync(join(dir, 'reddb.node'), addonBody)
      addon = true
    }

    writeFileSync(join(dir, 'package.json'), JSON.stringify(platformManifest(p, version, { addon }), null, 2) + '\n')
    built.push(dir)
  }
  return built
}

export function injectOptionalDependencies(manifestPath, version) {
  const json = JSON.parse(readFileSync(manifestPath, 'utf8'))
  json.optionalDependencies = { ...json.optionalDependencies }
  for (const p of PLATFORMS) json.optionalDependencies[packageName(p)] = version
  writeFileSync(manifestPath, JSON.stringify(json, null, 2) + '\n')
}

function parseArgs(argv) {
  const opts = { _: [] }
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i]
    if (a === '--allow-missing') opts.allowMissing = true
    else if (a.startsWith('--')) opts[a.slice(2)] = argv[++i]
    else opts._.push(a)
  }
  return opts
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [cmd, ...rest] = process.argv.slice(2)
  const opts = parseArgs(rest)
  try {
    if (cmd === 'build') {
      if (!opts.version || !opts.assets || !opts.out) throw new Error('build needs --version, --assets and --out')
      const dirs = buildPackages({
        version: opts.version,
        assetsDir: opts.assets,
        outDir: opts.out,
        allowMissing: opts.allowMissing,
      })
      if (dirs.length === 0) throw new Error('no platform packages were built')
      process.stdout.write(dirs.join('\n') + '\n')
    } else if (cmd === 'inject') {
      if (!opts.version || opts._.length === 0) throw new Error('inject needs --version and at least one package.json')
      for (const file of opts._) injectOptionalDependencies(file, opts.version)
    } else {
      throw new Error('usage: npm-platform-packages.mjs build|inject (see file header)')
    }
  } catch (err) {
    process.stderr.write(`npm-platform-packages: ${err.message}\n`)
    process.exit(1)
  }
}
