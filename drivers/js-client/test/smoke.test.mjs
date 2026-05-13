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
  const defaultHandlers = {
    'POST /query': () => readinessOk(),
  }
  const server = createServer(async (req, res) => {
    let body = ''
    req.on('data', (chunk) => { body += chunk })
    req.on('end', async () => {
      const key = `${req.method} ${req.url}`
      const handler = handlers[key] ?? defaultHandlers[key]
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

function readinessOk() {
  return {
    ok: true,
    statement: 'SELECT',
    affected: 0,
    columns: ['1'],
    rows: [{ 1: 1 }],
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

test('connect(http://) uses SELECT 1 readiness instead of degraded health', async () => {
  let readinessQueries = 0
  const stub = await startMockServer({
    'GET /health': () => ({
      status: 503,
      body: { ok: false, state: 'degraded', error: 'warming up' },
    }),
    'POST /query': (body) => {
      assert.equal(body.query, 'SELECT 1')
      readinessQueries += 1
      return readinessOk()
    },
  })
  try {
    const db = await connect(stub.baseUrl)
    assert.ok(db instanceof RedDB, 'returned RedDB')
    assert.equal(readinessQueries, 1)
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

test('connect(http://) sends query varargs and execute params', async () => {
  const seen = []
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock-0.0.0' }),
    'POST /query': (body) => {
      if (body.query === 'SELECT 1') {
        return readinessOk()
      }
      seen.push(body)
      return {
        ok: true,
        statement: body.query.startsWith('INSERT') ? 'INSERT' : 'SELECT',
        affected: body.query.startsWith('INSERT') ? 1 : 0,
        columns: [],
        rows: [],
      }
    },
  })
  try {
    const db = await connect(stub.baseUrl)
    await db.query('SELECT * FROM t WHERE id = $1 AND name = $2', 42, 'Ada')
    const inserted = await db.execute('INSERT INTO t (payload) VALUES ($1)', [
      new Uint8Array([0xde, 0xad]),
    ])

    assert.deepEqual(seen[0], {
      query: 'SELECT * FROM t WHERE id = $1 AND name = $2',
      params: [42, 'Ada'],
    })
    assert.deepEqual(seen[1], {
      query: 'INSERT INTO t (payload) VALUES ($1)',
      params: [{ $bytes: '3q0=' }],
    })
    assert.equal(inserted.affected, 1)
    await db.close()
  } finally {
    await stub.close()
  }
})

test('connect(http://) insert() returns affected count and assigned id', async () => {
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock-0.0.0' }),
    'POST /collections/users/rows': () => ({
      ok: true,
      id: 102,
      entity: { id: 102 },
    }),
  })
  try {
    const db = await connect(stub.baseUrl)
    const result = await db.insert('users', { name: 'Alice' })
    assert.equal(result.affected, 1)
    assert.equal(result.id, 102)
    await db.close()
  } finally {
    await stub.close()
  }
})

test('connect(http://) surfaces server errors as RedDBError', async () => {
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock-0.0.0' }),
    'POST /query': (body) => {
      if (body.query === 'SELECT 1') {
        return readinessOk()
      }
      return {
        status: 400,
        body: { ok: false, error_code: 'QUERY_ERROR', error: 'bad SQL' },
      }
    },
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
    'POST /query': (_body, req) => {
      seenAuth = req.headers['authorization'] ?? null
      return readinessOk()
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

test('domain clients expose explicit kv config and vault surfaces', async () => {
  const seen = []
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock-0.0.0' }),
    'POST /query': (body) => {
      if (body.query === 'SELECT 1') {
        return readinessOk()
      }
      seen.push(body.query)
      return {
        ok: true,
        statement: 'KEYED',
        affected: 1,
        columns: [],
        rows: [{ sql: body.query }],
      }
    },
  })
  try {
    const db = await connect(stub.baseUrl)
    assert.equal(typeof db.kv, 'function')
    await db.kv('sessions').put('token', 'abc')
    await db.config('app').put('api_key', null, {
      secretRef: { collection: 'secrets', key: 'api_key' },
    })
    await db.config('app').resolve('api_key')
    await db.vault('secrets').get('api_key')
    await db.vault('secrets').unseal('api_key')

    assert.equal(seen[0], "KV PUT sessions.token = 'abc'")
    assert.equal(seen[1], 'PUT CONFIG app api_key = SECRET_REF(vault, secrets.api_key)')
    assert.equal(seen[2], 'RESOLVE CONFIG app api_key')
    assert.equal(seen[3], 'VAULT GET secrets.api_key')
    assert.equal(seen[4], 'UNSEAL VAULT secrets.api_key')
    await db.close()
  } finally {
    await stub.close()
  }
})

test('config and vault clients reject volatile options before query dispatch', async () => {
  const seen = []
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock-0.0.0' }),
    'POST /query': (body) => {
      if (body.query === 'SELECT 1') {
        return readinessOk()
      }
      seen.push(body.query)
      return { ok: true, statement: 'KEYED', affected: 1, columns: [], rows: [] }
    },
  })
  try {
    const db = await connect(stub.baseUrl)
    assert.throws(
      () => db.config('app').put('temporary', 'x', { ttlMs: 1000 }),
      /config does not support TTL/,
    )
    assert.throws(
      () => db.vault('secrets').put('api_key', 'x', { expireMs: 1000 }),
      /vault does not support TTL/,
    )
    assert.deepEqual(seen, [])
    await db.close()
  } finally {
    await stub.close()
  }
})
