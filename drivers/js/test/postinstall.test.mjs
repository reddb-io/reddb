import assert from 'node:assert/strict'
import { execFile } from 'node:child_process'
import { mkdir, mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { promisify } from 'node:util'
import { test } from 'node:test'

const execFileAsync = promisify(execFile)
const repoRoot = path.resolve(import.meta.dirname, '..', '..', '..')

async function scaffoldPackage(fetcherSource) {
  const tempRoot = await mkdtemp(path.join(tmpdir(), 'reddb-postinstall-'))
  const packageDir = path.join(tempRoot, 'node_modules', '@reddb-io', 'sdk')
  await mkdir(path.join(packageDir, 'src', 'internal', 'asset-fetcher'), { recursive: true })
  await writeFile(
    path.join(packageDir, 'package.json'),
    JSON.stringify({ type: 'module', version: '1.0.5' }, null, 2),
  )
  await writeFile(
    path.join(packageDir, 'postinstall.js'),
    await readFile(path.join(repoRoot, 'drivers/js/postinstall.js'), 'utf8'),
  )
  await writeFile(
    path.join(packageDir, 'src/internal/asset-fetcher/index.js'),
    fetcherSource,
  )
  return { tempRoot, packageDir }
}

async function runPostinstall(packageDir, env = {}) {
  try {
    const { stdout, stderr } = await execFileAsync(process.execPath, ['postinstall.js'], {
      cwd: packageDir,
      env: { ...process.env, ...env },
    })
    return { code: 0, stdout, stderr }
  } catch (err) {
    return { code: err.code ?? 1, stdout: err.stdout ?? '', stderr: err.stderr ?? '' }
  }
}

function assertNamesEveryEscapeHatch(stderr) {
  assert.match(stderr, /REDDB_SKIP_POSTINSTALL=1/, 'must name REDDB_SKIP_POSTINSTALL')
  assert.match(stderr, /REDDB_BIN/, 'must name REDDB_BIN')
  assert.match(
    stderr,
    /curl -fsSL https:\/\/raw\.githubusercontent\.com\/reddb-io\/reddb\/main\/install\.sh \| bash/,
    'must name install.sh workspace-checkout convention',
  )
  assert.match(
    stderr,
    /https:\/\/github\.com\/reddb-io\/reddb\/releases/,
    'must name manual download URL',
  )
  assert.match(stderr, /cargo build --release --bin red/, 'must name workspace cargo build')
}

test('postinstall fails loud on 404 and names every escape hatch', async () => {
  const { tempRoot, packageDir } = await scaffoldPackage(
    [
      'export async function fetchReleaseAsset() {',
      "  const err = new Error('asset missing')",
      "  err.code = 'ASSET_NOT_FOUND'",
      "  err.url = 'https://github.com/reddb-io/reddb/releases/download/v1.0.5/red-linux-x86_64'",
      '  throw err',
      '}',
      '',
    ].join('\n'),
  )

  try {
    const { code, stderr } = await runPostinstall(packageDir, {
      REDDB_POSTINSTALL_VERSION: 'v1.0.5',
    })
    assert.notEqual(code, 0, 'install must fail loud on ASSET_NOT_FOUND')
    assert.match(stderr, /release asset not found/)
    assertNamesEveryEscapeHatch(stderr)
    const binFiles = await readdir(path.join(packageDir, 'bin')).catch(() => [])
    assert.deepEqual(binFiles, [], 'must not leave a partial bin/ on failure')
  } finally {
    await rm(tempRoot, { recursive: true, force: true })
  }
})

test('postinstall fails loud when offline (network error) and leaves bin/ empty', async () => {
  const { tempRoot, packageDir } = await scaffoldPackage(
    [
      'export async function fetchReleaseAsset() {',
      "  const err = new Error('getaddrinfo ENOTFOUND github.com')",
      "  err.code = 'ENOTFOUND'",
      '  throw err',
      '}',
      '',
    ].join('\n'),
  )

  try {
    const { code, stderr } = await runPostinstall(packageDir)
    assert.notEqual(code, 0, 'offline install must fail loud, not exit 0')
    assert.match(stderr, /postinstall could not download/i)
    assert.match(stderr, /ENOTFOUND/)
    assertNamesEveryEscapeHatch(stderr)
    const binFiles = await readdir(path.join(packageDir, 'bin')).catch(() => [])
    assert.deepEqual(binFiles, [], 'offline failure must leave bin/ empty (no half-written binary)')
  } finally {
    await rm(tempRoot, { recursive: true, force: true })
  }
})

test('postinstall skips quietly with REDDB_SKIP_POSTINSTALL=1', async () => {
  const { tempRoot, packageDir } = await scaffoldPackage(
    [
      'export async function fetchReleaseAsset() {',
      "  throw new Error('should not be called when skip is requested')",
      '}',
      '',
    ].join('\n'),
  )

  try {
    const { code, stdout } = await runPostinstall(packageDir, {
      REDDB_SKIP_POSTINSTALL: '1',
    })
    assert.equal(code, 0, 'explicit skip must exit 0')
    assert.match(stdout, /postinstall skipped/i)
  } finally {
    await rm(tempRoot, { recursive: true, force: true })
  }
})
