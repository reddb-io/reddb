/**
 * Mock-server tests for the KV API in @reddb-io/sdk.
 */

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { once } from 'node:events'

import { connect } from '../src/index.js'

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
        res.end(JSON.stringify({ ok: false, error: `no handler for ${key}` }))
        return
      }
      try {
        const parsed = body ? JSON.parse(body) : {}
        const out = await handler(parsed, req)
        res.statusCode = out?.status ?? 200
        if (out?.headers) {
          for (const [name, value] of Object.entries(out.headers)) {
            res.setHeader(name, value)
          }
        }
        if (out?.bodyText != null) {
          res.end(out.bodyText)
          return
        }
        res.setHeader('content-type', 'application/json')
        res.end(JSON.stringify(out?.body ?? out))
      } catch (err) {
        res.statusCode = 500
        res.end(JSON.stringify({ ok: false, error: String(err.message || err) }))
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

test('kv.put/get/delete use canonical /kv endpoint', async () => {
  const seen = []
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock' }),
    'PUT /collections/app/kv/theme': (body) => {
      seen.push(body)
      return { ok: true, id: 7, key: 'theme' }
    },
    'GET /collections/app/kv/theme': () => ({
      ok: true,
      collection: 'app',
      key: 'theme',
      value: 'dark',
      id: 7,
    }),
    'DELETE /collections/app/kv/theme': () => ({ ok: true, deleted: true, key: 'theme' }),
  })
  try {
    const db = await connect(stub.baseUrl)
    assert.deepEqual(await db.kv.put('app', 'theme', 'dark'), { ok: true, id: 7, key: 'theme' })
    assert.deepEqual(seen[0], { value: 'dark' })
    assert.equal((await db.kv.get('app', 'theme')).value, 'dark')
    assert.deepEqual(await db.kv.delete('app', 'theme'), { ok: true, deleted: true, key: 'theme' })
  } finally {
    await stub.close()
  }
})

test('kv.watch opens canonical SSE endpoint and yields parsed events', async () => {
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock' }),
    'GET /collections/app/kv/theme/watch': (_body, req) => {
      assert.equal(req.headers.accept, 'text/event-stream')
      return {
        headers: { 'content-type': 'text/event-stream' },
        bodyText:
          ': ready\n\n'
          + 'event: put\n'
          + 'data: {"collection":"app","key":"theme","value":"dark"}\n\n'
          + 'data: plain-text\n\n',
      }
    },
  })
  try {
    const db = await connect(stub.baseUrl)
    const events = []
    for await (const event of db.kv.watch('app', 'theme')) {
      events.push(event)
    }
    assert.deepEqual(events, [
      { collection: 'app', key: 'theme', value: 'dark' },
      'plain-text',
    ])
  } finally {
    await stub.close()
  }
})

test('kv HTTP route falls back to legacy /kvs endpoint on 404', async () => {
  const requests = []
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock' }),
    'PUT /collections/app/kvs/theme': () => ({ ok: true, id: 9, key: 'theme' }),
    '*': (_body, req) => {
      requests.push(`${req.method} ${req.url}`)
      return { status: 404, body: { ok: false, error: 'not found' } }
    },
  })
  try {
    const db = await connect(stub.baseUrl)
    const out = await db.kv.put('app', 'theme', 'dark')
    assert.equal(out.id, 9)
    assert.deepEqual(requests, ['PUT /collections/app/kv/theme'])
  } finally {
    await stub.close()
  }
})
