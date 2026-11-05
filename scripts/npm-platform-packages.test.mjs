import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdtempSync, readFileSync, statSync, writeFileSync, existsSync } from 'node:fs'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { fileURLToPath } from 'node:url'

import {
  PLATFORMS,
  addonAssetName,
  buildPackages,
  injectOptionalDependencies,
  packageName,
  parseSha256Sums,
} from './npm-platform-packages.mjs'
import { platformPackageNames } from '../drivers/js/src/platform-binary.js'

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const sha = (buf) => createHash('sha256').update(buf).digest('hex')

function stageAssets(assets, { tamper } = {}) {
  const dir = mkdtempSync(path.join(tmpdir(), 'reddb-assets-'))
  const lines = []
  for (const name of assets) {
    const body = Buffer.from(`binary:${name}`)
    writeFileSync(path.join(dir, name), tamper === name ? Buffer.from('tampered') : body)
    lines.push(`${sha(body)}  ${name}`)
  }
  writeFileSync(path.join(dir, 'SHA256SUMS'), lines.join('\n') + '\n')
  return dir
}

const required = PLATFORMS.filter((p) => p.required).map((p) => p.asset)

test('parseSha256Sums reads sha256sum output', () => {
  const sums = parseSha256Sums(`${'a'.repeat(64)}  red-linux-x86_64\n${'b'.repeat(64)} *red-windows-x86_64.exe\n`)
  assert.equal(sums.get('red-linux-x86_64'), 'a'.repeat(64))
  assert.equal(sums.get('red-windows-x86_64.exe'), 'b'.repeat(64))
})

test('buildPackages emits an installable package per present platform', () => {
  const assets = stageAssets([...required, 'red-windows-x86_64.exe'])
  const out = mkdtempSync(path.join(tmpdir(), 'reddb-out-'))
  const dirs = buildPackages({ version: '9.9.9', assetsDir: assets, outDir: out })
  assert.equal(dirs.length, required.length + 1)

  const musl = JSON.parse(readFileSync(path.join(out, 'red-linux-x64-musl', 'package.json'), 'utf8'))
  assert.equal(musl.name, '@reddb-io/red-linux-x64-musl')
  assert.equal(musl.version, '9.9.9')
  assert.deepEqual([musl.os, musl.cpu, musl.libc], [['linux'], ['x64'], ['musl']])
  assert.deepEqual(musl.files, ['bin/'])
  assert.equal(musl.scripts, undefined, 'platform packages must carry no install scripts')

  const win = JSON.parse(readFileSync(path.join(out, 'red-win32-x64', 'package.json'), 'utf8'))
  assert.equal(win.libc, undefined)
  assert.ok(existsSync(path.join(out, 'red-win32-x64', 'bin', 'red.exe')))

  const bin = path.join(out, 'red-linux-x64', 'bin', 'red')
  assert.equal(readFileSync(bin, 'utf8'), 'binary:red-linux-x86_64')
  assert.notEqual(statSync(bin).mode & 0o111, 0, 'binary must be executable')
})

test('buildPackages skips absent optional platforms but not required ones', () => {
  const out = mkdtempSync(path.join(tmpdir(), 'reddb-out-'))
  const ok = stageAssets(required)
  assert.equal(buildPackages({ version: '1.0.0', assetsDir: ok, outDir: out }).length, required.length)

  const missing = stageAssets(required.slice(1))
  assert.throws(() => buildPackages({ version: '1.0.0', assetsDir: missing, outDir: out }), /required asset missing/)
  assert.equal(
    buildPackages({ version: '1.0.0', assetsDir: missing, outDir: out, allowMissing: true }).length,
    required.length - 1,
  )
})

test('addonAssetName follows the release asset scheme', () => {
  const by = (key) => PLATFORMS.find((p) => p.key === key)
  assert.equal(addonAssetName(by('linux-x64')), 'reddb-node-linux-x86_64.node')
  assert.equal(addonAssetName(by('linux-x64-musl')), 'reddb-node-linux-x86_64-static.node')
  assert.equal(addonAssetName(by('darwin-arm64')), 'reddb-node-macos-aarch64.node')
  assert.equal(addonAssetName(by('win32-x64')), 'reddb-node-windows-x86_64.node')
})

test('buildPackages ships a verified addon as reddb.node and lists it in files', () => {
  const assets = stageAssets([...required, 'reddb-node-linux-x86_64.node'])
  const out = mkdtempSync(path.join(tmpdir(), 'reddb-out-'))
  buildPackages({ version: '1.0.0', assetsDir: assets, outDir: out })

  const withAddon = JSON.parse(readFileSync(path.join(out, 'red-linux-x64', 'package.json'), 'utf8'))
  assert.deepEqual(withAddon.files, ['bin/', 'reddb.node'])
  assert.equal(readFileSync(path.join(out, 'red-linux-x64', 'reddb.node'), 'utf8'), 'binary:reddb-node-linux-x86_64.node')

  const without = JSON.parse(readFileSync(path.join(out, 'red-linux-arm64', 'package.json'), 'utf8'))
  assert.deepEqual(without.files, ['bin/'], 'a platform with no addon asset still gets a plain package')
  assert.equal(existsSync(path.join(out, 'red-linux-arm64', 'reddb.node')), false)
})

test('buildPackages refuses an addon that does not match SHA256SUMS or has no entry', () => {
  const tampered = stageAssets([...required, 'reddb-node-linux-x86_64.node'], { tamper: 'reddb-node-linux-x86_64.node' })
  assert.throws(
    () => buildPackages({ version: '1.0.0', assetsDir: tampered, outDir: mkdtempSync(path.join(tmpdir(), 'reddb-out-')) }),
    /sha256 mismatch for reddb-node-linux-x86_64\.node/,
  )

  const unlisted = stageAssets(required)
  writeFileSync(path.join(unlisted, 'reddb-node-linux-x86_64.node'), 'x')
  assert.throws(
    () => buildPackages({ version: '1.0.0', assetsDir: unlisted, outDir: mkdtempSync(path.join(tmpdir(), 'reddb-out-')) }),
    /no SHA256SUMS entry for reddb-node-linux-x86_64\.node/,
  )
})

test('buildPackages refuses a binary that does not match SHA256SUMS', () => {
  const assets = stageAssets(required, { tamper: 'red-linux-x86_64' })
  const out = mkdtempSync(path.join(tmpdir(), 'reddb-out-'))
  assert.throws(() => buildPackages({ version: '1.0.0', assetsDir: assets, outDir: out }), /sha256 mismatch/)
})

test('buildPackages refuses an asset with no SHA256SUMS entry', () => {
  const assets = stageAssets(required)
  writeFileSync(path.join(assets, 'SHA256SUMS'), '')
  const out = mkdtempSync(path.join(tmpdir(), 'reddb-out-'))
  assert.throws(() => buildPackages({ version: '1.0.0', assetsDir: assets, outDir: out }), /no SHA256SUMS entry/)
})

test('injectOptionalDependencies pins every platform package to the release version', () => {
  const file = path.join(mkdtempSync(path.join(tmpdir(), 'reddb-inject-')), 'package.json')
  writeFileSync(file, JSON.stringify({ name: 'x', optionalDependencies: { keep: '^1.0.0' } }))
  injectOptionalDependencies(file, '2.3.4')
  const deps = JSON.parse(readFileSync(file, 'utf8')).optionalDependencies
  assert.equal(deps.keep, '^1.0.0')
  for (const p of PLATFORMS) assert.equal(deps[packageName(p)], '2.3.4')
})

test('publish-side and runtime package-name tables agree', () => {
  const runtime = new Set()
  for (const p of PLATFORMS) {
    const names = platformPackageNames({ platform: p.os, arch: p.cpu, libc: p.libc ?? null })
    assert.ok(names.includes(packageName(p)), `runtime resolver never looks for ${packageName(p)}`)
    runtime.add(packageName(p))
  }
  assert.equal(runtime.size, PLATFORMS.length, 'duplicate platform keys')
})

test('every platform asset maps to the release asset naming scheme', () => {
  const assetName = readFileSync(path.join(repoRoot, 'packages/internal-asset-fetcher/src/asset-name.js'), 'utf8')
  for (const p of PLATFORMS.filter((x) => !x.asset.endsWith('-static'))) {
    assert.ok(assetName.includes(p.asset.replace(/^red/, '${binName}')), `${p.asset} not in asset-name.js`)
  }
})
