import assert from 'node:assert/strict'
import { execFile } from 'node:child_process'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { promisify } from 'node:util'
import { test } from 'node:test'

const execFileAsync = promisify(execFile)
const repoRoot = path.resolve(import.meta.dirname, '..', '..', '..')

test('postinstall 404 explains manual install and REDDB_BIN fallback', async () => {
  const tempRoot = await mkdtemp(path.join(tmpdir(), 'reddb-postinstall-'))
  const packageDir = path.join(tempRoot, 'node_modules', '@reddb-io', 'sdk')

  try {
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

    const { stderr } = await execFileAsync(process.execPath, ['postinstall.js'], {
      cwd: packageDir,
      env: {
        ...process.env,
        REDDB_POSTINSTALL_VERSION: 'v1.0.5',
      },
    })

    assert.match(stderr, /release asset not found/)
    assert.match(stderr, /curl -fsSL https:\/\/raw\.githubusercontent\.com\/reddb-io\/reddb\/main\/install\.sh \| bash/)
    assert.match(stderr, /export REDDB_BIN="\$\(command -v red\)"/)
    assert.match(stderr, /REDDB_POSTINSTALL_VERSION=v1\.0\.5 npm rebuild @reddb-io\/sdk/)
    assert.match(stderr, /REDDB_SKIP_POSTINSTALL=1/)
  } finally {
    await rm(tempRoot, { recursive: true, force: true })
  }
})
