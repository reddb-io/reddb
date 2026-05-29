import { test } from 'node:test'
import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { once } from 'node:events'
import { Readable, pipeline } from 'node:stream'
import { promisify } from 'node:util'

import { connect } from '../src/index.js'
import { splitNdjson } from '../src/streaming.js'
import { RedDBError } from '../src/protocol.js'

const pipe = promisify(pipeline)

// A tiny configurable HTTP server. `routes` maps "<METHOD> <path>" to an
// async (req, res, body) handler. POST /query is stubbed so connect()'s
// `SELECT 1` probe succeeds without the caller wiring it every time.
async function startHttpServer(routes = {}) {
  const seen = { aborted: false }
  const server = createServer((req, res) => {
    const chunks = []
    req.on('data', (c) => chunks.push(c))
    req.on('aborted', () => {
      seen.aborted = true
    })
    req.on('end', async () => {
      const key = `${req.method} ${req.url}`
      const body = Buffer.concat(chunks).toString('utf8')
      const handler = routes[key] ?? defaults[key]
      if (!handler) {
        res.writeHead(404).end()
        return
      }
      try {
        await handler(req, res, body, seen)
      } catch (err) {
        if (!res.headersSent) res.writeHead(500)
        res.end(String(err))
      }
    })
  })
  server.listen(0, '127.0.0.1')
  await once(server, 'listening')
  const { port } = server.address()
  return {
    baseUrl: `http://127.0.0.1:${port}`,
    seen,
    async close() {
      await new Promise((resolve) => server.close(resolve))
    },
  }
}

const defaults = {
  'POST /query': (_req, res) => {
    res.writeHead(200, { 'content-type': 'application/json' })
    res.end(JSON.stringify({ ok: true, result: { statement: 'SELECT', affected: 0, rows: [] } }))
  },
}

function ndjson(...frames) {
  return frames.map((f) => JSON.stringify(f)).join('\n') + '\n'
}

function streamHead(res) {
  res.writeHead(200, {
    'content-type': 'application/x-ndjson',
    'transfer-encoding': 'chunked',
  })
}

test('http: for await over collection.stream() yields rows then exits cleanly', async () => {
  const srv = await startHttpServer({
    'POST /query/stream': (_req, res) => {
      streamHead(res)
      res.write(ndjson({ descriptor: { columns: [{ name: 'id' }, { name: 'name' }], schema_fingerprint: 'fp' } }))
      res.write(ndjson({ cursor: { token: 'tok-1', resumable: true } }))
      res.write(ndjson({ row: { id: 1, name: 'alice' } }))
      res.write(ndjson({ row: { id: 2, name: 'bob' } }))
      res.end(ndjson({ end: { row_count: 2 } }))
    },
  })
  try {
    const db = await connect(srv.baseUrl)
    const stream = db.collection('users').stream('SELECT id, name FROM users')
    const descriptors = []
    const cursors = []
    stream.on('descriptor', (d) => descriptors.push(d))
    stream.on('cursor', (c) => cursors.push(c))

    const rows = []
    for await (const row of stream) rows.push(row)

    assert.deepEqual(rows, [
      { id: 1, name: 'alice' },
      { id: 2, name: 'bob' },
    ])
    assert.equal(descriptors.length, 1)
    assert.equal(descriptors[0].schema_fingerprint, 'fp')
    assert.deepEqual(cursors[0], { token: 'tok-1', resumable: true })
    assert.equal(stream.endInfo.row_count, 2)
    await db.close()
  } finally {
    await srv.close()
  }
})

test('http: a mid-stream error frame propagates as a rejected iteration + error event', async () => {
  const srv = await startHttpServer({
    'POST /query/stream': (_req, res) => {
      streamHead(res)
      res.write(ndjson({ descriptor: { columns: [{ name: 'id' }] } }))
      res.write(ndjson({ row: { id: 1 } }))
      // Delay the error so the row is consumed before the failure lands —
      // an errored destroy() drops anything still buffered.
      setTimeout(() => {
        if (!res.writableEnded) res.end(ndjson({ error: { code: 'query_error', message: 'boom mid-stream' } }))
      }, 25)
    },
  })
  try {
    const db = await connect(srv.baseUrl)
    const stream = db.stream('SELECT id FROM users')
    const errorEvents = []
    stream.on('error', (e) => errorEvents.push(e))

    const rows = []
    await assert.rejects(
      (async () => {
        for await (const row of stream) rows.push(row)
      })(),
      (err) => err instanceof RedDBError && err.code === 'query_error',
    )
    // An errored teardown discards any row still buffered, so a `for await`
    // sees a prefix of the rows; what matters is the error propagation.
    assert.ok(rows.length <= 1, `unexpected rows before error: ${JSON.stringify(rows)}`)
    if (rows.length === 1) assert.deepEqual(rows[0], { id: 1 })
    assert.equal(errorEvents.length, 1)
    assert.equal(errorEvents[0].code, 'query_error')
    await db.close()
  } finally {
    await srv.close()
  }
})

test('http: a non-read-only refusal rejects before any frame', async () => {
  const srv = await startHttpServer({
    'POST /query/stream': (_req, res) => {
      res.writeHead(400, { 'content-type': 'application/json' })
      res.end(JSON.stringify({ ok: false, code: 'stream_unsupported_statement', statement_kind: 'mutation', error: 'INSERT is not streamable' }))
    },
  })
  try {
    const db = await connect(srv.baseUrl)
    const stream = db.stream("INSERT INTO users (id) VALUES (3)")
    await assert.rejects(
      (async () => {
        // eslint-disable-next-line no-unused-vars
        for await (const _row of stream) { /* should never yield */ }
      })(),
      (err) => err instanceof RedDBError && err.code === 'stream_unsupported_statement',
    )
    await db.close()
  } finally {
    await srv.close()
  }
})

test('http: cancel() mid-stream terminates the transport and rejects the iteration', async () => {
  let timer = null
  const srv = await startHttpServer({
    'POST /query/stream': (_req, res) => {
      streamHead(res)
      res.write(ndjson({ descriptor: { columns: [{ name: 'id' }] } }))
      res.write(ndjson({ row: { id: 1 } }))
      // Keep dribbling rows forever until the client aborts.
      let n = 2
      timer = setInterval(() => {
        if (!res.writableEnded) res.write(ndjson({ row: { id: n++ } }))
      }, 5)
      res.on('close', () => clearInterval(timer))
    },
  })
  try {
    const db = await connect(srv.baseUrl)
    const stream = db.stream('SELECT id FROM users')
    const rows = []
    await assert.rejects(
      (async () => {
        for await (const row of stream) {
          rows.push(row)
          if (rows.length === 1) await stream.cancel('done early')
        }
      })(),
      (err) => err instanceof RedDBError && err.code === 'STREAM_CANCELLED',
    )
    assert.equal(rows[0].id, 1)
    await db.close()
  } finally {
    if (timer) clearInterval(timer)
    await srv.close()
  }
})

test('http: inputStream ingests rows and resolves completion() with the terminal envelope', async () => {
  let received = null
  const srv = await startHttpServer({
    'POST /streams/input': (_req, res, body) => {
      const lines = body.split('\n').map((l) => l.trim()).filter(Boolean).map((l) => JSON.parse(l))
      const open = lines.find((l) => l.open)?.open
      const rows = lines.filter((l) => l.row).map((l) => l.row)
      received = { open, rows }
      res.writeHead(200, { 'content-type': 'application/x-ndjson' })
      res.end(ndjson({ end: { row_count: rows.length, committed_rid: 100, chunk_count: rows.length } }))
    },
  })
  try {
    const db = await connect(srv.baseUrl)
    const sink = db.collection('events').inputStream()
    sink.write({ id: 1, name: 'a' })
    sink.write({ id: 2, name: 'b' })
    sink.end()
    const end = await sink.completion()

    assert.equal(end.row_count, 2)
    assert.deepEqual(received.open, { target: 'events', columns: ['id', 'name'] })
    assert.deepEqual(received.rows, [{ id: 1, name: 'a' }, { id: 2, name: 'b' }])
    await db.close()
  } finally {
    await srv.close()
  }
})

test('http: createReadStream-style NDJSON piped through splitNdjson backpressures into inputStream', async () => {
  let rowCount = 0
  const srv = await startHttpServer({
    'POST /streams/input': (_req, res, body) => {
      const lines = body.split('\n').map((l) => l.trim()).filter(Boolean).map((l) => JSON.parse(l))
      rowCount = lines.filter((l) => l.row).length
      res.writeHead(200, { 'content-type': 'application/x-ndjson' })
      res.end(ndjson({ end: { row_count: rowCount } }))
    },
  })
  try {
    const db = await connect(srv.baseUrl)
    // Build an NDJSON source like a file would produce.
    const fileText = Array.from({ length: 50 }, (_, i) => JSON.stringify({ row: { id: i } })).join('\n') + '\n'
    const source = Readable.from([fileText], { objectMode: false })
    const sink = db.collection('rows').inputStream({ columns: ['id'] })
    await pipe(source, splitNdjson(), sink)
    const end = await sink.completion()
    assert.equal(end.row_count, 50)
    assert.equal(rowCount, 50)
    await db.close()
  } finally {
    await srv.close()
  }
})

test('http: Writable signals backpressure via write() returning false and a drain event', async () => {
  const srv = await startHttpServer({
    'POST /streams/input': (_req, res, body) => {
      const rows = body.split('\n').map((l) => l.trim()).filter(Boolean).map((l) => JSON.parse(l)).filter((l) => l.row)
      res.writeHead(200, { 'content-type': 'application/x-ndjson' })
      res.end(ndjson({ end: { row_count: rows.length } }))
    },
  })
  try {
    const db = await connect(srv.baseUrl)
    const sink = db.inputStream('bulk', { columns: ['id'] })
    let sawBackpressure = false
    // A tight synchronous burst overruns the object-mode buffer, so
    // write() must return false at least once.
    for (let i = 0; i < 500; i += 1) {
      if (!sink.write({ id: i })) sawBackpressure = true
    }
    assert.ok(sawBackpressure, 'write() must return false under a write burst')
    const drained = once(sink, 'drain')
    sink.end()
    await Promise.race([drained, sink.completion()])
    const end = await sink.completion()
    assert.equal(end.row_count, 500)
    await db.close()
  } finally {
    await srv.close()
  }
})

test('http: collection.query() stays a one-shot Promise (never hits /query/stream)', async () => {
  let streamHit = false
  const srv = await startHttpServer({
    'POST /query/stream': (_req, res) => {
      streamHit = true
      res.writeHead(500).end()
    },
    'POST /query': (_req, res) => {
      res.writeHead(200, { 'content-type': 'application/json' })
      res.end(JSON.stringify({ ok: true, result: { statement: 'SELECT', affected: 0, columns: ['id'], rows: [{ id: 7 }] } }))
    },
  })
  try {
    const db = await connect(srv.baseUrl)
    const result = await db.collection('users').query('SELECT id FROM users')
    assert.deepEqual(result.rows, [{ id: 7 }])
    assert.equal(streamHit, false)
    await db.close()
  } finally {
    await srv.close()
  }
})
