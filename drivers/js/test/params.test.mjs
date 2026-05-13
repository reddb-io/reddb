import { test } from 'node:test'
import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { once } from 'node:events'

import { connect, RedDBError } from '../src/index.js'

async function startMockServer(handlers) {
  const server = createServer(async (req, res) => {
    let body = ''
    req.on('data', (chunk) => { body += chunk })
    req.on('end', async () => {
      const handler = handlers[`${req.method} ${req.url}`]
      if (!handler) {
        res.statusCode = 404
        res.end(JSON.stringify({ ok: false, error: 'not found' }))
        return
      }
      const parsed = body ? JSON.parse(body) : {}
      const out = await handler(parsed, req)
      res.setHeader('content-type', 'application/json')
      res.end(JSON.stringify(out))
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

test('query(sql, ...params) sends HTTP params envelope', async () => {
  const seen = []
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock' }),
    'POST /query': (body) => {
      seen.push(body)
      return { ok: true, statement: 'SELECT', affected: 0, columns: [], rows: [] }
    },
  })
  try {
    const db = await connect(stub.baseUrl)
    await db.query(
      'SELECT * FROM t WHERE id = $1 AND name = $2 AND seen_at = $3',
      42,
      'Ada',
      new Date('2024-01-02T03:04:05.006Z'),
    )

    assert.equal(seen.length, 1)
    assert.equal(seen[0].query, 'SELECT * FROM t WHERE id = $1 AND name = $2 AND seen_at = $3')
    assert.deepEqual(seen[0].params.slice(0, 2), [42, 'Ada'])
    assert.deepEqual(seen[0].params[2], { $ts: '1704164645006000000' })
    await db.close()
  } finally {
    await stub.close()
  }
})

test('query(sql, paramsArray) remains supported', async () => {
  const seen = []
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock' }),
    'POST /query': (body) => {
      seen.push(body)
      return { ok: true, statement: 'SELECT', affected: 0, columns: [], rows: [] }
    },
  })
  try {
    const db = await connect(stub.baseUrl)
    await db.query('SELECT * FROM t WHERE payload = $1', [new Uint8Array([0xde, 0xad])])
    assert.deepEqual(seen[0].params, [{ $bytes: '3q0=' }])
    await db.close()
  } finally {
    await stub.close()
  }
})

test('execute aliases parameterized query', async () => {
  const seen = []
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock' }),
    'POST /query': (body) => {
      seen.push(body)
      return { ok: true, statement: 'INSERT', affected: 1, columns: [], rows: [] }
    },
  })
  try {
    const db = await connect(stub.baseUrl)
    const result = await db.execute('INSERT INTO t (id) VALUES ($1)', 7)
    assert.equal(result.affected, 1)
    assert.deepEqual(seen[0], { query: 'INSERT INTO t (id) VALUES ($1)', params: [7] })
    await db.close()
  } finally {
    await stub.close()
  }
})

test('unmappable params reject at call site', async () => {
  const stub = await startMockServer({
    'GET /health': () => ({ ok: true, version: 'mock' }),
    'POST /query': () => {
      throw new Error('query should not be dispatched')
    },
  })
  try {
    const db = await connect(stub.baseUrl)
    assert.throws(
      () => db.query('SELECT $1', Symbol('bad')),
      (err) => err instanceof RedDBError && err.code === 'UNSUPPORTED_PARAM',
    )
    await db.close()
  } finally {
    await stub.close()
  }
})
