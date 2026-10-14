// Exercise the shipped JavaScript remote client against a real RedDB server.
// RED_BIN is mandatory: never report a mock or skipped fixture as live coverage.
import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { once } from 'node:events'
import { mkdtemp, writeFile } from 'node:fs/promises'
import { createServer } from 'node:net'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { connect } from '../drivers/js-client/src/index.js'

const binary = process.env.RED_BIN
assert.ok(binary, 'RED_BIN must identify the built RedDB server')
const directory = await mkdtemp(join(tmpdir(), 'reddb-remote-cache-'))
const reservation = createServer()
reservation.listen(0, '127.0.0.1'); await once(reservation, 'listening')
const port = reservation.address().port
await new Promise(resolve => reservation.close(resolve))
const origin = `http://127.0.0.1:${port}`
const server = spawn(binary, ['server', '--path', join(directory, 'cache.rdb'), '--http-bind', `127.0.0.1:${port}`], {
  stdio: ['ignore', 'pipe', 'pipe'],
})
let logs = '', db
let spawnError
server.on('error', error => { spawnError = error })
server.stdout.on('data', text => { logs += text }); server.stderr.on('data', text => { logs += text })
try {
  let ready = false
  for (let attempt = 0; attempt < 150; attempt++) {
    if (spawnError) throw spawnError
    if (server.exitCode !== null) throw new Error('RedDB server exited before readiness')
    try {
      if ((await fetch(origin + '/health', { signal: AbortSignal.timeout(1000) })).ok) {
        ready = true
        break
      }
    } catch {}
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  assert.ok(ready, 'RedDB server must become healthy before cache verification')
  db = await connect(origin)
  const cache = db.cache
  const namespace = 'bigquery:organization/☃'
  const key = 'daily:2026-10?filters=all/a%20b'
  const payload = Buffer.from(JSON.stringify({ days: [{ day: '2026-10-14', tokens: 123 }] }))
  assert.equal(await cache.get(namespace, key), null)
  await cache.put(namespace, key, payload, { ttl_ms: 600_000, tags: ['organization:fixture', 'daily'] })
  assert.deepEqual(Buffer.from(await cache.get(namespace, key)), payload)
  assert.equal(await cache.exists(namespace, key), 'present')
  for (const name of ['tags', 'flush', 'invalidate-tags']) {
    await cache.put(namespace, name, new Uint8Array([0, 255, 128, 10]))
    assert.deepEqual([...await cache.get(namespace, name)], [0, 255, 128, 10])
    await cache.invalidate(namespace, name)
    assert.equal(await cache.get(namespace, name), null)
  }
  await cache.put(namespace, 'short', 'expires', { ttl_ms: 1 })
  await new Promise(resolve => setTimeout(resolve, 20))
  assert.equal(await cache.get(namespace, 'short'), null)
  assert.equal(await cache.invalidateTags(namespace, ['daily']), 1)
  assert.equal(await cache.get(namespace, key), null)
  await cache.put(namespace, 'prefix/a', 'a')
  await cache.put(namespace, 'prefix/b', 'b')
  await cache.put('other', 'prefix/a', 'independent')
  assert.equal(await cache.invalidatePrefix(namespace, 'prefix/'), 2)
  assert.equal(Buffer.from(await cache.get('other', 'prefix/a')).toString(), 'independent')
  await cache.put(namespace, 'last', 'value')
  await cache.flushNamespace(namespace)
  assert.equal(await cache.get(namespace, 'last'), null)
  console.log(JSON.stringify({ nativeCacheRemoteSdk: 'passed', tenMinuteTtl: 'accepted', binaryRoundtrip: true, expiration: true, allInvalidationMethods: true, artifacts: directory }))
} finally {
  await db?.close()
  if (server.exitCode === null && !spawnError) {
    const stopped = once(server, 'close')
    const timeout = setTimeout(() => server.kill('SIGKILL'), 5000)
    server.kill('SIGTERM')
    try { await stopped } finally { clearTimeout(timeout) }
  }
  await writeFile(join(directory, 'server.log'), logs)
}
