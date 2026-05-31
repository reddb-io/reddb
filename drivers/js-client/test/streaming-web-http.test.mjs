/**
 * Web-streaming over the real `fetch` transport (#876).
 *
 * The parity suite proves wrapper behaviour against mock sessions; this file
 * proves the Web implementation drives the *actual* Web-native HTTP transport
 * — reading `fetch`'s `response.body` ReadableStream for SELECT, streaming a
 * chunked request body for input, and aborting the `fetch` on cancel. The Web
 * streaming impl is injected into the transport-agnostic core `RedDB`, exactly
 * as a browser entry would wire it.
 */

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { once } from 'node:events'

import { RedDB as CoreRedDB } from '../src/core/index.js'
import { HttpRpcClient } from '../src/http.js'
import { createSelectStream, createInputStream } from '../src/streaming-web.js'
import { RedDBError } from '../src/protocol.js'

const WEB_STREAMING = { createSelectStream, createInputStream }

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
      const handler = routes[key]
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

function ndjson(...frames) {
  return frames.map((f) => JSON.stringify(f)).join('\n') + '\n'
}

function streamHead(res) {
  res.writeHead(200, {
    'content-type': 'application/x-ndjson',
    'transfer-encoding': 'chunked',
  })
}

function webDb(baseUrl) {
  return new CoreRedDB(new HttpRpcClient({ baseUrl }), WEB_STREAMING)
}

test('web/http: SELECT over fetch response.body yields typed frames in order', async () => {
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
    const db = webDb(srv.baseUrl)
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

test('web/http: a mid-stream error frame rejects the iteration', async () => {
  const srv = await startHttpServer({
    'POST /query/stream': (_req, res) => {
      streamHead(res)
      res.write(ndjson({ descriptor: { columns: [{ name: 'id' }] } }))
      res.write(ndjson({ row: { id: 1 } }))
      setTimeout(() => {
        if (!res.writableEnded) res.end(ndjson({ error: { code: 'query_error', message: 'boom mid-stream' } }))
      }, 25)
    },
  })
  try {
    const db = webDb(srv.baseUrl)
    const stream = db.stream('SELECT id FROM users')
    const rows = []
    await assert.rejects(
      (async () => {
        for await (const row of stream) rows.push(row)
      })(),
      (err) => err instanceof RedDBError && err.code === 'query_error',
    )
    assert.ok(rows.length <= 1, `unexpected rows before error: ${JSON.stringify(rows)}`)
    await db.close()
  } finally {
    await srv.close()
  }
})

test('web/http: a non-read-only refusal rejects before any frame', async () => {
  const srv = await startHttpServer({
    'POST /query/stream': (_req, res) => {
      res.writeHead(400, { 'content-type': 'application/json' })
      res.end(JSON.stringify({ ok: false, code: 'stream_unsupported_statement', error: 'INSERT is not streamable' }))
    },
  })
  try {
    const db = webDb(srv.baseUrl)
    const stream = db.stream('INSERT INTO users (id) VALUES (3)')
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

test('web/http: cancel() aborts the underlying fetch and rejects the iteration', async () => {
  let timer = null
  const srv = await startHttpServer({
    'POST /query/stream': (_req, res, _body, seen) => {
      streamHead(res)
      res.write(ndjson({ row: { id: 1 } }))
      let n = 2
      timer = setInterval(() => {
        if (!res.writableEnded) res.write(ndjson({ row: { id: n++ } }))
      }, 5)
      // The fetch abort severs the response mid-stream: res closes before it
      // ends normally. That early close is the server-side proof of abort.
      res.on('close', () => {
        clearInterval(timer)
        if (!res.writableEnded) seen.responseAborted = true
      })
    },
  })
  try {
    const db = webDb(srv.baseUrl)
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
    // Give the abort a moment to land server-side.
    await new Promise((r) => setTimeout(r, 30))
    assert.ok(srv.seen.responseAborted, 'cancel() must abort the underlying fetch (server saw the response severed mid-stream)')
    await db.close()
  } finally {
    if (timer) clearInterval(timer)
    await srv.close()
  }
})

test('web/http: inputStream ingests rows and resolves completion() with the terminal envelope', async () => {
  let received = null
  const srv = await startHttpServer({
    'POST /streams/input': (_req, res, body) => {
      const lines = body.split('\n').map((l) => l.trim()).filter(Boolean).map((l) => JSON.parse(l))
      const open = lines.find((l) => l.open)?.open
      const rows = lines.filter((l) => l.row).map((l) => l.row)
      received = { open, rows }
      res.writeHead(200, { 'content-type': 'application/x-ndjson' })
      res.end(ndjson({ end: { row_count: rows.length, committed_rid: 100 } }))
    },
  })
  try {
    const db = webDb(srv.baseUrl)
    const sink = db.collection('events').inputStream()
    sink.write({ id: 1, name: 'a' })
    sink.write({ id: 2, name: 'b' })
    sink.end()
    const end = await sink.completion()

    assert.equal(end.row_count, 2)
    assert.equal(end.committed_rid, 100)
    assert.deepEqual(received.open, { target: 'events', columns: ['id', 'name'] })
    assert.deepEqual(received.rows, [
      { id: 1, name: 'a' },
      { id: 2, name: 'b' },
    ])
    await db.close()
  } finally {
    await srv.close()
  }
})
