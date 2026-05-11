/**
 * Mock-server tests for the cache API in @reddb-io/sdk.
 *
 * Verifies the correct HTTP method, URL, and body for each cache call
 * without needing a compiled `red` binary or live server.
 */

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { once } from 'node:events'

import { connect } from '../src/index.js'

// ---------------------------------------------------------------------------
// Mock server helper
// ---------------------------------------------------------------------------

async function startMockServer(handlers) {
  const server = createServer(async (req, res) => {
    let body = ''
    req.on('data', (chunk) => { body += chunk })
    req.on('end', async () => {
      const key = `${req.method} ${req.url}`
      const handler = handlers[key] ?? handlers['*']
      if (!handler) {
        res.statusCode = 404
        res.setHeader('content-type', 'application/json')
        res.end(JSON.stringify({ error: `no handler for ${key}` }))
        return
      }
      try {
        const parsed = body ? JSON.parse(body) : {}
        const out = await handler(parsed, req)
        res.statusCode = out?.statusCode ?? 200
        res.setHeader('content-type', 'application/json')
        res.end(JSON.stringify(out?.body ?? out))
      } catch (err) {
        res.statusCode = 500
        res.end(JSON.stringify({ error: String(err.message || err) }))
      }
    })
  })
  server.listen(0, '127.0.0.1')
  await once(server, 'listening')
  const { port } = server.address()
  return {
    baseUrl: `http://127.0.0.1:${port}`,
    close: () => new Promise((resolve) => server.close(resolve)),
  }
}

function b64(str) {
  return Buffer.from(str).toString('base64')
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test('cache.get returns Uint8Array on hit', async () => {
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock' }),
    'GET /cache/ns/my-ns/my-key': () => ({ value: b64('hello') }),
  })
  try {
    const db = await connect(stub.baseUrl)
    const result = await db.cache.get('my-ns', 'my-key')
    assert.ok(result instanceof Uint8Array)
    assert.equal(Buffer.from(result).toString('utf8'), 'hello')
  } finally {
    await stub.close()
  }
})

test('cache.get returns null on miss', async () => {
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock' }),
    'GET /cache/ns/my-ns/missing': () => ({ value: null }),
  })
  try {
    const db = await connect(stub.baseUrl)
    const result = await db.cache.get('my-ns', 'missing')
    assert.equal(result, null)
  } finally {
    await stub.close()
  }
})

test('cache.put sends PUT with base64-encoded string value', async () => {
  let captured = null
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock' }),
    'PUT /cache/ns/ns1/k1': (body) => {
      captured = body
      return { ok: true }
    },
  })
  try {
    const db = await connect(stub.baseUrl)
    await db.cache.put('ns1', 'k1', 'world', { ttl_ms: 5000, tags: ['t1'] })
    assert.ok(captured)
    assert.equal(captured.value, b64('world'))
    assert.equal(captured.ttl_ms, 5000)
    assert.deepEqual(captured.tags, ['t1'])
  } finally {
    await stub.close()
  }
})

test('cache.put encodes Uint8Array value', async () => {
  let captured = null
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock' }),
    'PUT /cache/ns/ns1/k2': (body) => {
      captured = body
      return { ok: true }
    },
  })
  try {
    const db = await connect(stub.baseUrl)
    await db.cache.put('ns1', 'k2', new Uint8Array([1, 2, 3, 4]))
    assert.deepEqual(Array.from(Buffer.from(captured.value, 'base64')), [1, 2, 3, 4])
  } finally {
    await stub.close()
  }
})

test('cache.exists returns present', async () => {
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock' }),
    'GET /cache/ns/ns1/k1/exists': () => ({ status: 'present' }),
  })
  try {
    const db = await connect(stub.baseUrl)
    assert.equal(await db.cache.exists('ns1', 'k1'), 'present')
  } finally {
    await stub.close()
  }
})

test('cache.exists returns absent', async () => {
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock' }),
    'GET /cache/ns/ns1/gone/exists': () => ({ status: 'absent' }),
  })
  try {
    const db = await connect(stub.baseUrl)
    assert.equal(await db.cache.exists('ns1', 'gone'), 'absent')
  } finally {
    await stub.close()
  }
})

test('cache.invalidate sends DELETE to entry URL', async () => {
  let hit = false
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock' }),
    'DELETE /cache/ns/ns1/k1': () => {
      hit = true
      return { ok: true }
    },
  })
  try {
    const db = await connect(stub.baseUrl)
    await db.cache.invalidate('ns1', 'k1')
    assert.ok(hit)
  } finally {
    await stub.close()
  }
})

test('cache.invalidatePrefix returns removed count', async () => {
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock' }),
    'DELETE /cache/ns/ns1?prefix=usr%3A': () => ({ removed: 7 }),
  })
  try {
    const db = await connect(stub.baseUrl)
    const count = await db.cache.invalidatePrefix('ns1', 'usr:')
    assert.equal(count, 7)
  } finally {
    await stub.close()
  }
})

test('cache.invalidateTags sends tags in body', async () => {
  let captured = null
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock' }),
    'DELETE /cache/ns/ns1/tags': (body) => {
      captured = body
      return { removed: 2 }
    },
  })
  try {
    const db = await connect(stub.baseUrl)
    const count = await db.cache.invalidateTags('ns1', ['alpha', 'beta'])
    assert.equal(count, 2)
    assert.deepEqual(captured.tags, ['alpha', 'beta'])
  } finally {
    await stub.close()
  }
})

test('cache.flushNamespace POSTs to /admin/blob_cache/flush_namespace', async () => {
  let captured = null
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock' }),
    'POST /admin/blob_cache/flush_namespace': (body) => {
      captured = body
      return { ok: true }
    },
  })
  try {
    const db = await connect(stub.baseUrl)
    await db.cache.flushNamespace('ns1')
    assert.equal(captured.namespace, 'ns1')
  } finally {
    await stub.close()
  }
})

test('db.cache is a CacheClient instance', async () => {
  const { CacheClient } = await import('../src/cache.js')
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock' }),
  })
  try {
    const db = await connect(stub.baseUrl)
    assert.ok(db.cache instanceof CacheClient)
  } finally {
    await stub.close()
  }
})
