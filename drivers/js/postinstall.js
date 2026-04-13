/**
 * postinstall.js — download the matching `red` binary from GitHub Releases.
 *
 * Behavior:
 *   - Resolves `red-<platform>-<arch>` from `process.platform` + `process.arch`.
 *   - Targets the GitHub release matching this package's version.
 *   - Drops the binary at `<package>/bin/red[.exe]` and chmods +x on Unix.
 *   - On any failure, prints a warning to stderr but exits 0 — `npm install`
 *     never breaks because of this script. The user gets a clear error
 *     later when they call `connect()` if the binary isn't present.
 *
 * Override hooks (env vars):
 *   REDDB_SKIP_POSTINSTALL=1      do nothing
 *   REDDB_POSTINSTALL_VERSION=…   pull a different release tag
 *   REDDB_POSTINSTALL_REPO=…      pull from a fork (default: forattini-dev/reddb)
 */

import { createRequire } from 'node:module'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import { existsSync, mkdirSync, writeFileSync, chmodSync } from 'node:fs'
import { request as httpsRequest } from 'node:https'

const HERE = dirname(fileURLToPath(import.meta.url))
const require = createRequire(import.meta.url)
const pkg = require('./package.json')

const DEFAULT_REPO = 'forattini-dev/reddb'

if (process.env.REDDB_SKIP_POSTINSTALL === '1') {
  process.stdout.write('reddb: postinstall skipped (REDDB_SKIP_POSTINSTALL=1)\n')
  process.exit(0)
}

main().catch((err) => {
  process.stderr.write(
    `reddb: postinstall could not download the binary (${err.message}).\n` +
      `       The package will still install. To use the driver you can:\n` +
      `         - set REDDB_BINARY_PATH=/path/to/red\n` +
      `         - or install the binary manually from https://github.com/${DEFAULT_REPO}/releases\n`,
  )
  process.exit(0)
})

async function main() {
  const assetName = resolveAssetName()
  const repo = process.env.REDDB_POSTINSTALL_REPO || DEFAULT_REPO
  const tag = process.env.REDDB_POSTINSTALL_VERSION
    ? normalizeTag(process.env.REDDB_POSTINSTALL_VERSION)
    : `v${pkg.version}`

  const binDir = join(HERE, 'bin')
  const binaryPath = join(binDir, defaultBinaryName())

  if (existsSync(binaryPath)) {
    process.stdout.write(`reddb: binary already present at ${binaryPath}\n`)
    return
  }

  const url = `https://github.com/${repo}/releases/download/${tag}/${assetName}`
  process.stdout.write(`reddb: downloading ${url}\n`)

  const body = await downloadFollowingRedirects(url)
  mkdirSync(binDir, { recursive: true })
  writeFileSync(binaryPath, body)
  if (process.platform !== 'win32') {
    chmodSync(binaryPath, 0o755)
  }
  process.stdout.write(`reddb: installed binary at ${binaryPath}\n`)
}

function defaultBinaryName() {
  return process.platform === 'win32' ? 'red.exe' : 'red'
}

function resolveAssetName() {
  const platform = process.platform
  const arch = process.arch
  if (platform === 'linux' && arch === 'x64') return 'red-linux-x86_64'
  if (platform === 'linux' && arch === 'arm64') return 'red-linux-aarch64'
  if (platform === 'linux' && (arch === 'arm' || arch === 'armv7l')) return 'red-linux-armv7'
  if (platform === 'darwin' && arch === 'x64') return 'red-macos-x86_64'
  if (platform === 'darwin' && arch === 'arm64') return 'red-macos-aarch64'
  if (platform === 'win32' && arch === 'x64') return 'red-windows-x86_64.exe'
  throw new Error(`unsupported platform/arch combination: ${platform}/${arch}`)
}

function normalizeTag(value) {
  const v = String(value).trim()
  return v.startsWith('v') ? v : `v${v}`
}

function downloadFollowingRedirects(url, depth = 0) {
  if (depth > 5) {
    return Promise.reject(new Error('too many redirects'))
  }
  return new Promise((resolve, reject) => {
    const req = httpsRequest(
      url,
      {
        method: 'GET',
        headers: {
          'User-Agent': `reddb-driver/${pkg.version}`,
          Accept: 'application/octet-stream',
        },
      },
      (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          res.resume()
          downloadFollowingRedirects(res.headers.location, depth + 1).then(resolve, reject)
          return
        }
        if (res.statusCode < 200 || res.statusCode >= 300) {
          res.resume()
          reject(new Error(`HTTP ${res.statusCode} fetching ${url}`))
          return
        }
        const chunks = []
        res.on('data', (chunk) => chunks.push(chunk))
        res.on('end', () => resolve(Buffer.concat(chunks)))
        res.on('error', reject)
      },
    )
    req.on('error', reject)
    req.end()
  })
}
