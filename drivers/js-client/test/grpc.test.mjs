import { test } from 'node:test'
import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { createServer as createHttp2Server } from 'node:http2'
import { createServer as createNetServer } from 'node:net'
import { once } from 'node:events'
import { mkdtemp, rm } from 'node:fs/promises'
import { existsSync } from 'node:fs'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

import { connect } from '../src/index.js'

test('connect(grpc://) uses gRPC instead of RedWire framing', async () => {
  let seenPath = null
  const server = createHttp2Server()
  server.on('stream', (stream, headers) => {
    seenPath = headers[':path']
    stream.respond({
      ':status': 200,
      'content-type': 'application/grpc',
      'grpc-status': '0',
    })
    stream.end(grpcMessage(queryReply({
      statement: 'select',
      columns: ['n'],
      result_json: JSON.stringify({
        statement: 'select',
        affected: 0,
        columns: ['n'],
        records: [{ n: 1 }],
      }),
    })))
  })
  server.listen(0, '127.0.0.1')
  await once(server, 'listening')
  const { port } = server.address()

  try {
    const db = await connect(`grpc://127.0.0.1:${port}`)
    const result = await db.query('SELECT 1 AS n')
    assert.equal(seenPath, '/reddb.v1.RedDb/Query')
    assert.deepEqual(result.columns, ['n'])
    assert.deepEqual(result.rows, [{ n: 1 }])
    await db.close()
  } finally {
    await new Promise((resolve) => server.close(resolve))
  }
})

test(
  'connect(grpc://) SELECT and INSERT work against a live server',
  { skip: process.env.RED_SMOKE === '1' ? false : 'set RED_SMOKE=1 to run live gRPC smoke' },
  async () => {
    const bin = redBinary()
    if (!bin) {
      throw new Error('RED_SMOKE=1 requires RED_BIN or target/debug/red')
    }

    const port = await pickFreePort()
    const dir = await mkdtemp(path.join(tmpdir(), 'reddb-js-grpc-'))
    const dataPath = path.join(dir, 'data.db')
    const server = spawn(bin, [
      'server',
      '--path',
      dataPath,
      '--grpc',
      '--grpc-bind',
      `127.0.0.1:${port}`,
    ], {
      stdio: 'ignore',
    })

    try {
      const db = await waitForGrpc(port)
      await db.query('CREATE TABLE js_grpc_rows (id INT, name TEXT)')
      await db.query("INSERT INTO js_grpc_rows (id, name) VALUES (1, 'Ada')")
      await db.query("INSERT INTO js_grpc_rows (id, name) VALUES (2, 'Grace')")

      const one = await db.query('SELECT 1')
      assert.deepEqual(one.columns, ['LIT:1'])
      assert.deepEqual(one.rows, [{ 'LIT:1': 1 }])

      const many = await db.query('SELECT id, name FROM js_grpc_rows ORDER BY id')
      assert.deepEqual(many.columns, ['id', 'name'])
      assert.equal(many.rows.length, 2)
      assert.equal(many.rows[0].id, 1)
      assert.equal(many.rows[0].name, 'Ada')
      assert.equal(many.rows[1].id, 2)
      assert.equal(many.rows[1].name, 'Grace')

      await db.close()
    } finally {
      server.kill()
      await once(server, 'close').catch(() => {})
      await rm(dir, { recursive: true, force: true })
    }
  },
)

function grpcMessage(message) {
  const out = new Uint8Array(5 + message.length)
  const view = new DataView(out.buffer)
  out[0] = 0
  view.setUint32(1, message.length, false)
  out.set(message, 5)
  return out
}

function queryReply({ statement, columns, result_json }) {
  const fields = [
    boolField(1, true),
    stringField(3, statement),
    uint64Field(6, 1n),
    stringField(7, result_json),
  ]
  for (const column of columns) fields.push(stringField(5, column))
  return concat(fields)
}

function stringField(no, value) {
  const bytes = new TextEncoder().encode(String(value))
  return concat([varint((BigInt(no) << 3n) | 2n), varint(BigInt(bytes.length)), bytes])
}

function boolField(no, value) {
  return concat([varint((BigInt(no) << 3n) | 0n), varint(value ? 1n : 0n)])
}

function uint64Field(no, value) {
  return concat([varint((BigInt(no) << 3n) | 0n), varint(BigInt(value))])
}

function varint(value) {
  let n = BigInt(value)
  const bytes = []
  while (n >= 0x80n) {
    bytes.push(Number((n & 0x7fn) | 0x80n))
    n >>= 7n
  }
  bytes.push(Number(n))
  return Uint8Array.from(bytes)
}

function concat(parts) {
  const total = parts.reduce((sum, part) => sum + part.length, 0)
  const out = new Uint8Array(total)
  let pos = 0
  for (const part of parts) {
    out.set(part, pos)
    pos += part.length
  }
  return out
}

function redBinary() {
  if (process.env.RED_BIN && existsSync(process.env.RED_BIN)) return process.env.RED_BIN
  const fallback = fileURLToPath(new URL('../../../target/debug/red', import.meta.url))
  return existsSync(fallback) ? fallback : null
}

async function waitForGrpc(port) {
  const deadline = Date.now() + 10_000
  let lastError = null
  while (Date.now() < deadline) {
    const db = await connect(`grpc://127.0.0.1:${port}`)
    try {
      await db.query('SELECT 1')
      return db
    } catch (err) {
      lastError = err
      await db.close().catch(() => {})
      await new Promise((resolve) => setTimeout(resolve, 50))
    }
  }
  throw lastError ?? new Error('server did not accept gRPC connections')
}

async function pickFreePort() {
  const listener = createNetServer()
  listener.listen(0, '127.0.0.1')
  await once(listener, 'listening')
  const { port } = listener.address()
  await new Promise((resolve) => listener.close(resolve))
  return port
}
