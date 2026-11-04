import assert from 'node:assert/strict'
import { before, after, test } from 'node:test'
import { execFileSync, spawn } from 'node:child_process'
import { once } from 'node:events'
import { createInterface } from 'node:readline'
import { createServer } from 'node:http'
import { mkdtemp, readFile, writeFile, rm } from 'node:fs/promises'
import { readFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { resolve, dirname, extname, sep } from 'node:path'
import { fileURLToPath } from 'node:url'
import { chromium, firefox } from 'playwright'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..')
const prefix = '/packages/browser/src/'
const password = 'native browser interop master'
const browsers = { chromium, firefox }
const selected = (process.env.REDDB_BROWSER_BROWSERS || 'chromium,firefox').split(',').filter(name => browsers[name])
let binary, temporary, server, origin

before(async () => {
  if (process.env.REDDB_LOCAL_SERVICE_BINARY) binary = process.env.REDDB_LOCAL_SERVICE_BINARY
  else {
  const toolchain = readFileSync(resolve(root, 'rust-toolchain.toml'), 'utf8').match(/channel\s*=\s*"([^"]+)"/)[1]
  execFileSync('cargo', [`+${toolchain}`, 'build', '--locked', '-p', 'reddb-io-engine', '--example', 'local_workspace_service'], { cwd: root, stdio: 'inherit' })
  const metadata = JSON.parse(execFileSync('cargo', [`+${toolchain}`, 'metadata', '--no-deps', '--format-version', '1'], { cwd: root, encoding: 'utf8' }))
  binary = resolve(metadata.target_directory, 'debug/examples/local_workspace_service' + (process.platform === 'win32' ? '.exe' : ''))
  }
  temporary = await mkdtemp(resolve(tmpdir(), 'reddb-native-vault-'))
  server = createServer(async (request, response) => {
    if (request.url === '/') return response.writeHead(200, { 'Content-Type': 'text/html' }).end('<!doctype html><title>Local Vault interop</title>')
    const path = resolve(root, `.${new URL(request.url, 'http://localhost').pathname}`)
    if (!path.startsWith(root + sep)) return response.writeHead(403).end()
    try { const body = await readFile(path); response.writeHead(200, { 'Content-Type': extname(path) === '.wasm' ? 'application/wasm' : 'text/javascript' }).end(body) }
    catch { response.writeHead(404).end() }
  })
  server.listen(0, '127.0.0.1'); await once(server, 'listening')
  origin = `http://127.0.0.1:${server.address().port}`
})

after(async () => {
  if (server?.listening) await new Promise(resolve => server.close(resolve))
  if (temporary) await rm(temporary, { recursive: true, force: true })
})

async function start(path, credential = password) {
  const child = spawn(binary, [...(process.env.REDDB_LOCAL_SERVICE_IS_RED ? ['workspace-store'] : []), '--path', path], { stdio: ['pipe', 'pipe', 'pipe'] })
  const lines = createInterface({ input: child.stdout })
  const ready = new Promise((resolve, reject) => {
    lines.once('line', line => {
      try { resolve(JSON.parse(line)) } catch { reject(new Error('Invalid private handshake')) }
    })
    child.once('exit', () => reject(new Error('Private service exited before readiness')))
    child.once('error', reject)
  })
  child.stdin.write(JSON.stringify({ password: credential }) + '\n')
  let handshake
  try { handshake = await ready }
  catch (error) { child.stdin.destroy(); throw error }
  const call = async (method, params = {}, headers = {}) => {
    const response = await fetch(`http://127.0.0.1:${handshake.port}/workspace`, {
      method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${handshake.token}`, ...headers },
      body: JSON.stringify({ method, params }),
    })
    return response.status === 200 ? response.json() : { status: response.status }
  }
  let stopped = false
  return {
    call,
    async stop(graceful = true) {
      if (stopped) return
      stopped = true
      const exited = once(child, 'exit')
      if (graceful) await call('shutdown')
      child.stdin.end()
      await exited
      lines.close()
    },
  }
}

test('native service enforces private ownership, rejects Origin, and recovers atomic credentials', { timeout: 120_000 }, async () => {
  const path = resolve(temporary, 'native.rdb')
  let service = await start(path)
  try {
    assert.equal((await service.call('read', {}, { Authorization: 'Bearer wrong' })).status, 403)
    assert.equal((await service.call('read', {}, { Origin: 'https://untrusted.invalid' })).status, 403)
    assert.equal((await service.call('query', { sql: 'SELECT 1' })).ok, false)
    await assert.rejects(start(path), /exited before readiness/)
    assert.equal((await service.call('commit', { metadata: { draft: 'SELECT 42', profile: 'admin' }, secrets: { admin: 'native private credential' } })).ok, true)
    assert.equal((await service.call('vaultLock')).ok, true)
    assert.equal((await service.call('commit', { metadata: { profile: 'must roll back' }, secrets: { admin: 'abandoned credential' } })).ok, false)
    assert.equal((await service.call('read')).result.profile, 'admin')
    assert.equal((await service.call('vaultUnlock', { password: 'wrong', create: true })).ok, false)
    await service.stop(false) // stdin EOF is the parent-process lifeline
    service = await start(path)
    assert.equal((await service.call('vaultReveal', { name: 'admin' })).result, 'native private credential')
    const bytes = await readFile(path)
    for (const secret of [password, 'native private credential', 'abandoned credential']) assert.equal(bytes.includes(Buffer.from(secret)), false)
  } finally { await service.stop() }
  const corrupt = resolve(temporary, 'corrupt.rdb')
  const bytes = Buffer.from('an existing corrupt file that must never be recreated')
  await writeFile(corrupt, bytes)
  await assert.rejects(start(corrupt), /exited before readiness/)
  assert.deepEqual(await readFile(corrupt), bytes)
})

for (const name of selected) test(`${name}: native to WASM to native Vault and mixed transaction compatibility`, { timeout: 120_000 }, async () => {
  const path = resolve(temporary, `${name}.rdb`)
  let service = await start(path)
  await service.call('commit', { metadata: { draft: 'native draft' }, secrets: { admin: 'credential from native' } })
  await service.stop()
  const backup = Array.from(await readFile(path))
  const browser = await browsers[name].launch({ headless: true })
  try {
    const page = await browser.newPage(); await page.goto(origin)
    const result = await page.evaluate(async ({ prefix, backup, password }) => {
      const { restoreDatabase } = await import(prefix + 'index.js')
      const db = await restoreDatabase({ name: 'native-vault-import', backup: new Uint8Array(backup) })
      try {
        const status = await db.vault.status()
        await db.vault.unlock(password)
        const credential = await db.vault.reveal('admin')
        const draft = await db.query('SELECT entry_value FROM workspace_metadata WHERE entry_key = $1', ['draft'])
        await db.query('BEGIN')
        await db.query('UPDATE workspace_metadata SET entry_value = $1 WHERE entry_key = $2', ['browser draft', 'draft'])
        await db.vault.put('admin', 'credential from browser')
        await db.query('COMMIT')
        return { status, credential, draft, backup: Array.from(await db.exportBackup()) }
      } finally { await db.close() }
    }, { prefix, backup, password })
    assert.deepEqual(result.status, { initialized: true, locked: true })
    assert.equal(result.credential, 'credential from native')
    assert.deepEqual(result.draft.rows, [['native draft']])
    await writeFile(path, new Uint8Array(result.backup))
    service = await start(path)
    assert.equal((await service.call('read')).result.draft, 'browser draft')
    assert.equal((await service.call('vaultReveal', { name: 'admin' })).result, 'credential from browser')
    await service.stop()
  } finally { await browser.close(); await service.stop() }
})
