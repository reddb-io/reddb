/**
 * Smoke test for the thin client.
 *
 * Stands up a mock HTTP RedDB endpoint (just `health` + `query`),
 * connects through `connect('http://...')`, and exercises the public
 * `RedDB` surface end-to-end. Verifies the JSON-RPC envelope shape
 * the driver emits without needing a compiled `red`/`red_client`
 * binary.
 */

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { once } from 'node:events'

import { connect, RedDB, RedDBError } from '../src/index.js'

/**
 * Spin up a tiny in-process HTTP RPC stub that mimics the relevant
 * RedDB endpoints. Returns `{ baseUrl, close }`.
 */
async function startMockServer(handlers) {
  const server = createServer(async (req, res) => {
    let body = ''
    req.on('data', (chunk) => { body += chunk })
    req.on('end', async () => {
      const handler = handlers[`${req.method} ${req.url}`]
      if (!handler) {
        res.statusCode = 404
        res.setHeader('content-type', 'application/json')
        res.end(JSON.stringify({ ok: false, error: 'not found' }))
        return
      }
      try {
        const parsed = body ? JSON.parse(body) : {}
        const out = await handler(parsed, req)
        res.statusCode = out?.status ?? 200
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

test('connect(http://) returns a RedDB handle', async () => {
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock-0.0.0' }),
  })
  try {
    const db = await connect(stub.baseUrl)
    assert.ok(db instanceof RedDB, 'returned RedDB')
    await db.close()
  } finally {
    await stub.close()
  }
})

test('connect(http://) round-trips query() through the mock', async () => {
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock-0.0.0' }),
    'POST /query': (body) => ({
      ok: true,
      statement: 'SELECT',
      affected: 0,
      columns: ['n'],
      rows: [{ n: 1, sql: body.query }],
    }),
  })
  try {
    const db = await connect(stub.baseUrl)
    const result = await db.query('SELECT 1 AS n')
    assert.equal(result.statement, 'SELECT')
    assert.equal(result.rows.length, 1)
    assert.equal(result.rows[0].n, 1)
    assert.equal(result.rows[0].sql, 'SELECT 1 AS n')
    await db.close()
  } finally {
    await stub.close()
  }
})

test('connect(http://) surfaces server errors as RedDBError', async () => {
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock-0.0.0' }),
    'POST /query': () => ({
      status: 400,
      body: { ok: false, error_code: 'QUERY_ERROR', error: 'bad SQL' },
    }),
  })
  try {
    const db = await connect(stub.baseUrl)
    await assert.rejects(
      () => db.query('NOT VALID'),
      (err) => {
        assert.ok(err instanceof RedDBError, 'is RedDBError')
        assert.equal(err.code, 'QUERY_ERROR')
        return true
      },
    )
    await db.close()
  } finally {
    await stub.close()
  }
})

test('connect(http://) honours auth.token (Bearer header)', async () => {
  let seenAuth = null
  const stub = await startMockServer({
    'GET /health': (_body, req) => {
      seenAuth = req.headers['authorization'] ?? null
      return { ok: true, version: 'mock-0.0.0' }
    },
  })
  try {
    const db = await connect(stub.baseUrl, { auth: { token: 'sk-test-123' } })
    assert.equal(seenAuth, 'Bearer sk-test-123')
    await db.close()
  } finally {
    await stub.close()
  }
})
