import { test } from 'node:test'
import assert from 'node:assert/strict'
import { createServer } from 'node:net'
import { once } from 'node:events'

import { connect } from '../src/index.js'
import { RedDBError } from '../src/protocol.js'

// Frame kinds we exercise (mirror src/redwire.js MessageKind).
const K = {
  Hello: 0x10,
  HelloAck: 0x11,
  AuthResponse: 0x13,
  AuthOk: 0x14,
  StreamEnd: 0x25,
  OpenStream: 0x29,
  OpenAck: 0x2a,
  StreamChunk: 0x2b,
  StreamError: 0x2c,
  StreamCancel: 0x2d,
}

function encodeFrame(kind, corr, obj, streamId = 0) {
  const payload = obj == null ? Buffer.alloc(0) : Buffer.from(JSON.stringify(obj), 'utf8')
  const buf = Buffer.alloc(16 + payload.length)
  buf.writeUInt32LE(buf.length, 0)
  buf[4] = kind
  buf[5] = 0
  buf.writeUInt16LE(streamId, 6)
  buf.writeBigUInt64LE(BigInt(corr), 8)
  payload.copy(buf, 16)
  return buf
}

/**
 * A fake RedWire TCP server. Handles the [magic+version] prefix and the
 * Hello/Auth handshake internally; delegates stream frames to `onFrame`.
 * `onFrame(frame, ctx)` where ctx has `{ send(kind, obj, streamId), socket }`.
 */
async function startFakeRedwire(onFrame) {
  const server = createServer((socket) => {
    let buf = Buffer.alloc(0)
    let gotPrefix = false
    const send = (kind, obj, streamId = 0, corr = 0) =>
      socket.write(encodeFrame(kind, corr, obj, streamId))
    socket.on('data', (chunk) => {
      buf = Buffer.concat([buf, chunk])
      if (!gotPrefix) {
        if (buf.length < 2) return
        buf = buf.subarray(2) // discard [MAGIC, version]
        gotPrefix = true
      }
      while (buf.length >= 16) {
        const len = buf.readUInt32LE(0)
        if (buf.length < len) break
        const kind = buf[4]
        const streamId = buf.readUInt16LE(6)
        const corr = buf.readBigUInt64LE(8)
        const payloadBuf = buf.subarray(16, len)
        buf = buf.subarray(len)
        const payload = payloadBuf.length ? JSON.parse(payloadBuf.toString('utf8')) : null
        const frame = { kind, streamId, corr, payload }
        if (kind === K.Hello) {
          send(K.HelloAck, { auth: 'anonymous', features: 0 }, 0, corr)
        } else if (kind === K.AuthResponse) {
          send(K.AuthOk, { features: 0 }, 0, corr)
        } else {
          onFrame(frame, { socket, send: (k, o, sid) => send(k, o, sid, corr) })
        }
      }
    })
    socket.on('error', () => {})
  })
  server.listen(0, '127.0.0.1')
  await once(server, 'listening')
  const { port } = server.address()
  return {
    url: `red://127.0.0.1:${port}`,
    async close() {
      await new Promise((resolve) => server.close(resolve))
    },
  }
}

test('redwire: for await over stream() yields rows and exits cleanly', async () => {
  const srv = await startFakeRedwire((frame, ctx) => {
    if (frame.kind === K.OpenStream) {
      const sid = frame.streamId
      ctx.send(K.OpenAck, { lease_handle: '7', snapshot_lsn: 12, resumable: true }, sid)
      ctx.send(K.StreamChunk, { seq: 0, rows: [{ id: 1, name: 'ada' }], terminal: false }, sid)
      ctx.send(K.StreamChunk, { seq: 1, rows: [{ id: 2, name: 'grace' }], terminal: false }, sid)
      ctx.send(K.StreamEnd, { stats: { row_count: 2, lease_id: 7, snapshot_lsn: 12, cancelled: false } }, sid)
    }
  })
  try {
    const db = await connect(srv.url)
    const rows = []
    for await (const row of db.collection('people').stream('SELECT id, name FROM people')) {
      rows.push(row)
    }
    assert.deepEqual(rows, [
      { id: 1, name: 'ada' },
      { id: 2, name: 'grace' },
    ])
    await db.close()
  } finally {
    await srv.close()
  }
})

test('redwire: a StreamError frame propagates as a rejected iteration', async () => {
  const srv = await startFakeRedwire((frame, ctx) => {
    if (frame.kind === K.OpenStream) {
      const sid = frame.streamId
      ctx.send(K.OpenAck, { lease_handle: '1', snapshot_lsn: 1, resumable: false }, sid)
      ctx.send(K.StreamChunk, { seq: 0, rows: [{ id: 1 }], terminal: false }, sid)
      // Delay the error so the row is consumed before the failure lands.
      setTimeout(() => ctx.send(K.StreamError, { code: 'query_error', message: 'kaboom' }, sid), 25)
    }
  })
  let db
  try {
    db = await connect(srv.url)
    const stream = db.stream('SELECT id FROM people')
    const rows = []
    const errorEvents = []
    stream.on('error', (e) => errorEvents.push(e))
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
    assert.equal(errorEvents[0].code, 'query_error')
  } finally {
    await db?.close()
    await srv.close()
  }
})

test('redwire: cancel() mid-stream emits StreamCancel and rejects the iteration', async () => {
  let timer = null
  let cancelled = false
  const srv = await startFakeRedwire((frame, ctx) => {
    if (frame.kind === K.OpenStream) {
      const sid = frame.streamId
      ctx.send(K.OpenAck, { lease_handle: '1', snapshot_lsn: 1, resumable: false }, sid)
      ctx.send(K.StreamChunk, { seq: 0, rows: [{ id: 1 }], terminal: false }, sid)
      let n = 2
      timer = setInterval(() => ctx.send(K.StreamChunk, { seq: n, rows: [{ id: n++ }], terminal: false }, sid), 5)
    } else if (frame.kind === K.StreamCancel) {
      cancelled = true
      clearInterval(timer)
      ctx.send(K.StreamEnd, { stats: { row_count: 1, cancelled: true } }, frame.streamId)
    }
  })
  try {
    const db = await connect(srv.url)
    const stream = db.stream('SELECT id FROM people')
    const rows = []
    await assert.rejects(
      (async () => {
        for await (const row of stream) {
          rows.push(row)
          if (rows.length === 1) await stream.cancel('enough')
        }
      })(),
      (err) => err instanceof RedDBError && err.code === 'STREAM_CANCELLED',
    )
    assert.equal(rows[0].id, 1)
    // Give the StreamCancel frame a moment to land on the fake server.
    await new Promise((r) => setTimeout(r, 30))
    assert.ok(cancelled, 'server must have received a StreamCancel frame')
    await db.close()
  } finally {
    if (timer) clearInterval(timer)
    await srv.close()
  }
})

test('redwire: inputStream ingests chunked rows and resolves completion()', async () => {
  let received = null
  const srv = await startFakeRedwire((frame, ctx) => {
    const sid = frame.streamId
    if (frame.kind === K.OpenStream) {
      received = { open: frame.payload, rows: [] }
      ctx.send(K.OpenAck, { lease_handle: '9', snapshot_lsn: 3, resumable: false }, sid)
    } else if (frame.kind === K.StreamChunk) {
      for (const row of frame.payload.rows ?? []) received.rows.push(row)
      if (frame.payload.terminal) {
        ctx.send(
          K.StreamEnd,
          { stats: { row_count: received.rows.length, chunk_count: 2, committed_rid: 42, snapshot_lsn: 3, cancelled: false } },
          sid,
        )
      }
    }
  })
  try {
    const db = await connect(srv.url)
    const sink = db.collection('ingest').inputStream()
    sink.write({ id: 1, v: 'a' })
    sink.write({ id: 2, v: 'b' })
    sink.end()
    const end = await sink.completion()

    assert.equal(end.row_count, 2)
    assert.equal(end.committed_rid, 42)
    assert.equal(received.open.direction, 'in')
    assert.equal(received.open.target, 'ingest')
    assert.deepEqual(received.open.columns, ['id', 'v'])
    assert.deepEqual(received.rows, [{ id: 1, v: 'a' }, { id: 2, v: 'b' }])
    await db.close()
  } finally {
    await srv.close()
  }
})
