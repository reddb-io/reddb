import assert from 'node:assert/strict'
import { mkdtemp, mkdir, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'

import { NativeRpcClient, resolveNativeAddon } from '../src/native-engine.js'
import { RedDBError } from '../src/protocol.js'

/** Engine double: records the order requests arrive and answers by method. */
function fakeEngine(handler = (req) => ({ jsonrpc: '2.0', id: req.id, result: { ok: req.method } })) {
  const seen = []
  let closed = 0
  return {
    seen,
    get closed() {
      return closed
    },
    async call(line) {
      const req = JSON.parse(line)
      seen.push(req.method)
      // Earlier requests are slower: without chaining they would finish last.
      await new Promise((r) => setTimeout(r, req.method === 'first' ? 20 : 1))
      return JSON.stringify(handler(req))
    },
    close() {
      closed++
    },
  }
}

test('requests reach the engine in call order even when not awaited', async () => {
  const engine = fakeEngine()
  const client = new NativeRpcClient(engine)
  const results = await Promise.all([client.call('first'), client.call('second'), client.call('third')])
  assert.deepEqual(engine.seen, ['first', 'second', 'third'])
  assert.deepEqual(results, [{ ok: 'first' }, { ok: 'second' }, { ok: 'third' }])
})

test('an error envelope rejects with a RedDBError carrying code and data', async () => {
  const engine = fakeEngine((req) => ({
    jsonrpc: '2.0',
    id: req.id,
    error: { code: 'TX_ALREADY_OPEN', message: 'transaction 1 already open', data: { tx: 1 } },
  }))
  const client = new NativeRpcClient(engine)
  await assert.rejects(client.call('tx.begin'), (err) => {
    assert.ok(err instanceof RedDBError)
    assert.equal(err.code, 'TX_ALREADY_OPEN')
    assert.deepEqual(err.data, { tx: 1 })
    return true
  })
})

test('one failing request does not wedge the queue', async () => {
  let n = 0
  const engine = fakeEngine((req) =>
    n++ === 0 ? { jsonrpc: '2.0', id: req.id, error: { code: 'X', message: 'boom' } } : { jsonrpc: '2.0', id: req.id, result: 1 },
  )
  const client = new NativeRpcClient(engine)
  const [a, b] = await Promise.allSettled([client.call('a'), client.call('b')])
  assert.equal(a.status, 'rejected')
  assert.equal(b.status, 'fulfilled')
})

test('close sends the close RPC, releases the engine once, and rejects later calls', async () => {
  const engine = fakeEngine()
  const client = new NativeRpcClient(engine)
  await client.close()
  await client.close()
  assert.deepEqual(engine.seen, ['close'])
  assert.equal(engine.closed, 1)
  await assert.rejects(client.call('query'), (err) => err.code === 'CLIENT_CLOSED')
})

test('a close RPC issued by the caller also marks the client closed', async () => {
  const client = new NativeRpcClient(fakeEngine())
  await client.call('close')
  await assert.rejects(client.call('query'), (err) => err.code === 'CLIENT_CLOSED')
})

test('resolveNativeAddon honours REDDB_NATIVE_ADDON before any package', () => {
  assert.equal(resolveNativeAddon({ env: { REDDB_NATIVE_ADDON: '/x/reddb.node' }, names: [] }), '/x/reddb.node')
})

test('resolveNativeAddon finds reddb.node in the platform package, else null', async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'reddb-addon-'))
  const dir = path.join(root, 'red-linux-x64')
  await mkdir(dir, { recursive: true })
  await writeFile(path.join(dir, 'package.json'), '{}')
  const resolve = (id) => {
    if (id === '@reddb-io/red-linux-x64/package.json') return path.join(dir, 'package.json')
    throw new Error('MODULE_NOT_FOUND')
  }
  const names = ['@reddb-io/red-linux-x64']
  assert.equal(resolveNativeAddon({ resolve, env: {}, names }), null, 'package without reddb.node')
  await writeFile(path.join(dir, 'reddb.node'), '')
  assert.equal(resolveNativeAddon({ resolve, env: {}, names }), path.join(dir, 'reddb.node'))
})
