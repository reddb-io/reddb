import { test } from 'node:test'
import assert from 'node:assert/strict'

import { KvClient } from '../src/index.js'

function fakeKv(responder) {
  const calls = []
  const client = {
    async call(method, params) {
      calls.push({ method, params })
      return responder?.(method, params) ?? null
    },
  }
  return { kv: new KvClient(client), calls }
}

test('kv.put quotes namespaced keys without rewriting', async () => {
  const { kv, calls } = fakeKv()
  await kv.put('corpus:version', '1.0.0')
  assert.equal(calls[0].method, 'query')
  assert.equal(calls[0].params.sql, "KV PUT kv_default.'corpus:version' = '1.0.0'")
})

test('kv.put preserves supported keys in generated SQL', async () => {
  const { kv, calls } = fakeKv()
  await kv.put('corpus_version', '1.0.0')
  assert.equal(calls[0].method, 'query')
  assert.equal(calls[0].params.sql, "KV PUT kv_default.corpus_version = '1.0.0'")
})

test('kv.get returns stored value and null on miss', async () => {
  const { kv, calls } = fakeKv((_, params) => {
    if (params.sql.endsWith('.missing')) return { rows: [{ value: null }] }
    return { rows: [{ value: '1.0.0' }] }
  })
  assert.equal(await kv.get('corpus_version'), '1.0.0')
  assert.equal(await kv.get('missing'), null)
  assert.deepEqual(calls.map((call) => call.params.sql), [
    'KV GET kv_default.corpus_version',
    'KV GET kv_default.missing',
  ])
})

test('kv.get quotes namespaced keys without rewriting', async () => {
  const { kv, calls } = fakeKv(() => ({ rows: [{ value: '1.0.0' }] }))
  assert.equal(await kv.get('corpus:version'), '1.0.0')
  assert.deepEqual(calls.map((call) => call.params.sql), [
    "KV GET kv_default.'corpus:version'",
  ])
})

test('kv.get supports collection instances and options collection', async () => {
  const calls = []
  const client = {
    async call(method, params) {
      calls.push({ method, params })
      return { rows: [{ value: 'hit' }] }
    },
  }
  await new KvClient(client, 'sessions').get('abc123')
  await new KvClient(client).get('abc123', { collection: 'sessions' })
  assert.deepEqual(calls.map((call) => call.params.sql), [
    'KV GET sessions.abc123',
    'KV GET sessions.abc123',
  ])
})

test('kv.getMany preserves input order', async () => {
  const values = { a: 1, b: null, c: 3 }
  const { kv } = fakeKv((_, params) => {
    const key = params.sql.split('.').at(-1)
    return { rows: [{ value: values[key] }] }
  })
  assert.deepEqual(await kv.getMany(['c', 'a', 'b']), [3, 1, null])
})
