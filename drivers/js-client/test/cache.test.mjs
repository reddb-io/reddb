/**
 * Mock-server tests for the cache API in @reddb-io/client.
 *
 * No binary or live server required — a tiny in-process HTTP stub
 * verifies the correct HTTP method, URL, and body for each cache call.
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

test('cache.put sends PUT with base64-encoded value', async () => {
  let captured = null
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock' }),
    'PUT /cache/ns/ns1/k1': (body, req) => {
      captured = { body, url: req.url }
      return { ok: true }
    },
  })
  try {
    const db = await connect(stub.baseUrl)
    await db.cache.put('ns1', 'k1', 'world', { ttl_ms: 5000 })
    assert.ok(captured)
    assert.equal(captured.body.value, b64('world'))
    assert.equal(captured.body.ttl_ms, 5000)
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
    const bytes = new Uint8Array([1, 2, 3, 4])
    await db.cache.put('ns1', 'k2', bytes)
    assert.ok(captured)
    const decoded = Buffer.from(captured.value, 'base64')
    assert.deepEqual(Array.from(decoded), [1, 2, 3, 4])
  } finally {
    await stub.close()
  }
})

test('cache.exists returns status field', async () => {
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock' }),
    'GET /cache/ns/ns1/k1/exists': () => ({ status: 'present' }),
  })
  try {
    const db = await connect(stub.baseUrl)
    const status = await db.cache.exists('ns1', 'k1')
    assert.equal(status, 'present')
  } finally {
    await stub.close()
  }
})

test('cache.exists returns maybe when status missing', async () => {
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock' }),
    'GET /cache/ns/ns1/gone/exists': () => ({}),
  })
  try {
    const db = await connect(stub.baseUrl)
    const status = await db.cache.exists('ns1', 'gone')
    assert.equal(status, 'maybe')
  } finally {
    await stub.close()
  }
})

test('cache.invalidate sends DELETE to entry URL', async () => {
  let method = null
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock' }),
    'DELETE /cache/ns/ns1/k1': (body, req) => {
      method = req.method
      return { ok: true }
    },
  })
  try {
    const db = await connect(stub.baseUrl)
    await db.cache.invalidate('ns1', 'k1')
    assert.equal(method, 'DELETE')
  } finally {
    await stub.close()
  }
})

test('cache.invalidatePrefix sends DELETE with prefix query param', async () => {
  let captured = null
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock' }),
    'DELETE /cache/ns/ns1?prefix=usr%3A': (body, req) => {
      captured = req.url
      return { removed: 3 }
    },
  })
  try {
    const db = await connect(stub.baseUrl)
    const count = await db.cache.invalidatePrefix('ns1', 'usr:')
    assert.equal(count, 3)
    assert.ok(captured.includes('prefix='))
  } finally {
    await stub.close()
  }
})

test('cache.invalidateTags sends DELETE with tags body', async () => {
  let captured = null
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock' }),
    'DELETE /cache/ns/ns1/tags': (body) => {
      captured = body
      return { removed: 5 }
    },
  })
  try {
    const db = await connect(stub.baseUrl)
    const count = await db.cache.invalidateTags('ns1', ['tag-a', 'tag-b'])
    assert.equal(count, 5)
    assert.deepEqual(captured.tags, ['tag-a', 'tag-b'])
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
