/**
 * Unit tests for the cache client wrapper.
 *
 * @reddb-io/sdk is embedded-only at connect() time; these tests use a
 * direct fake client so cache encoding/dispatch remains covered without
 * reintroducing a remote HTTP connect path.
 */

import { test } from 'node:test'
import assert from 'node:assert/strict'

import { CacheClient, RedDB } from '../src/index.js'

function b64(str) {
  return Buffer.from(str).toString('base64')
}

function fakeCache(responder) {
  const calls = []
  const client = {
    async call(method, params) {
      calls.push({ method, params })
      return responder?.(method, params) ?? null
    },
  }
  return { cache: new CacheClient(client, 'http'), calls }
}

test('cache.get returns Uint8Array on hit', async () => {
  const { cache } = fakeCache(() => ({ value: b64('hello') }))
  const result = await cache.get('my-ns', 'my-key')
  assert.ok(result instanceof Uint8Array)
  assert.equal(Buffer.from(result).toString('utf8'), 'hello')
})

test('cache.get returns null on miss', async () => {
  const { cache } = fakeCache(() => ({ value: null }))
  assert.equal(await cache.get('my-ns', 'missing'), null)
})

test('cache.put sends base64-encoded string value', async () => {
  const { cache, calls } = fakeCache()
  await cache.put('ns1', 'k1', 'world', { ttl_ms: 5000, tags: ['t1'] })
  assert.deepEqual(calls[0], {
    method: 'cache.put',
    params: {
      namespace: 'ns1',
      key: 'k1',
      value: b64('world'),
      ttl_ms: 5000,
      tags: ['t1'],
    },
  })
})

test('cache.put encodes Uint8Array value', async () => {
  const { cache, calls } = fakeCache()
  await cache.put('ns1', 'k2', new Uint8Array([1, 2, 3, 4]))
  assert.deepEqual(Array.from(Buffer.from(calls[0].params.value, 'base64')), [1, 2, 3, 4])
})

test('cache.exists returns present', async () => {
  const { cache } = fakeCache(() => ({ status: 'present' }))
  assert.equal(await cache.exists('ns1', 'k1'), 'present')
})

test('cache.exists returns absent', async () => {
  const { cache } = fakeCache(() => ({ status: 'absent' }))
  assert.equal(await cache.exists('ns1', 'gone'), 'absent')
})

test('cache.invalidate dispatches cache.invalidate', async () => {
  const { cache, calls } = fakeCache()
  await cache.invalidate('ns1', 'k1')
  assert.deepEqual(calls[0], {
    method: 'cache.invalidate',
    params: { namespace: 'ns1', key: 'k1' },
  })
})

test('cache.invalidatePrefix returns removed count', async () => {
  const { cache, calls } = fakeCache(() => ({ removed: 7 }))
  assert.equal(await cache.invalidatePrefix('ns1', 'usr:'), 7)
  assert.deepEqual(calls[0], {
    method: 'cache.invalidate_prefix',
    params: { namespace: 'ns1', prefix: 'usr:' },
  })
})

test('cache.invalidateTags sends tags', async () => {
  const { cache, calls } = fakeCache(() => ({ removed: 2 }))
  assert.equal(await cache.invalidateTags('ns1', ['alpha', 'beta']), 2)
  assert.deepEqual(calls[0], {
    method: 'cache.invalidate_tags',
    params: { namespace: 'ns1', tags: ['alpha', 'beta'] },
  })
})

test('cache.flushNamespace dispatches cache.flush_namespace', async () => {
  const { cache, calls } = fakeCache()
  await cache.flushNamespace('ns1')
  assert.deepEqual(calls[0], {
    method: 'cache.flush_namespace',
    params: { namespace: 'ns1' },
  })
})

test('cache methods throw UNSUPPORTED_TRANSPORT on embedded transport', async () => {
  const calls = []
  const fakeClient = {
    call(method, params) {
      calls.push({ method, params })
      return Promise.resolve(null)
    },
  }
  const cache = new CacheClient(fakeClient, 'embedded')
  const methods = [
    () => cache.get('ns', 'k'),
    () => cache.put('ns', 'k', 'v'),
    () => cache.exists('ns', 'k'),
    () => cache.invalidate('ns', 'k'),
    () => cache.invalidatePrefix('ns', 'p'),
    () => cache.invalidateTags('ns', ['t']),
    () => cache.flushNamespace('ns'),
  ]
  for (const fn of methods) {
    await assert.rejects(fn(), (err) => {
      assert.equal(err.name, 'RedDBError')
      assert.equal(err.code, 'UNSUPPORTED_TRANSPORT')
      assert.match(err.message, /embedded/)
      return true
    })
  }
  assert.equal(calls.length, 0, 'no RPC calls should be issued')
})

test('db.cache is a CacheClient instance', () => {
  const db = new RedDB({ call() {}, close() {} }, { transport: 'embedded' })
  assert.ok(db.cache instanceof CacheClient)
})
