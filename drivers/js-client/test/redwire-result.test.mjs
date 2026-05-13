import { test } from 'node:test'
import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { createServer } from 'node:net'
import { once } from 'node:events'
import { mkdtemp, rm } from 'node:fs/promises'
import { existsSync } from 'node:fs'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

import { connect } from '../src/index.js'
import { BinaryTag, decodeResultPayload } from '../src/redwire.js'
import { RedDBError } from '../src/protocol.js'

test('RedWire Result JSON payloads still pass through unchanged', () => {
  const payload = new TextEncoder().encode(JSON.stringify({
    ok: true,
    statement: 'INSERT',
    affected: 1,
  }))

  assert.deepEqual(decodeResultPayload(payload), {
    ok: true,
    statement: 'INSERT',
    affected: 1,
  })
})

test('RedWire Result binary payload decodes SELECT 1 rows and columns', () => {
  const payload = encodeBinaryResult(['1'], [
    [[BinaryTag.I64, 1]],
  ])

  assert.deepEqual(decodeResultPayload(payload), {
    ok: true,
    statement: 'SELECT',
    affected: 0,
    columns: ['1'],
    rows: [{ 1: 1 }],
  })
})

test('RedWire Result binary payload decodes every row and column', () => {
  const large = BigInt(Number.MAX_SAFE_INTEGER) + 2n
  const payload = encodeBinaryResult(['id', 'name', 'active', 'score', 'big', 'empty'], [
    [
      [BinaryTag.I64, 1],
      [BinaryTag.Text, 'Ada'],
      [BinaryTag.Bool, true],
      [BinaryTag.F64, 9.5],
      [BinaryTag.U64, large],
      [BinaryTag.Null, null],
    ],
    [
      [BinaryTag.I64, 2],
      [BinaryTag.Text, 'Grace'],
      [BinaryTag.Bool, false],
      [BinaryTag.F64, 10.25],
      [BinaryTag.U64, 42n],
      [BinaryTag.Null, null],
    ],
  ])

  assert.deepEqual(decodeResultPayload(payload), {
    ok: true,
    statement: 'SELECT',
    affected: 0,
    columns: ['id', 'name', 'active', 'score', 'big', 'empty'],
    rows: [
      { id: 1, name: 'Ada', active: true, score: 9.5, big: large, empty: null },
      { id: 2, name: 'Grace', active: false, score: 10.25, big: 42, empty: null },
    ],
  })
})

test('RedWire Result binary payload rejects truncated rows', () => {
  const payload = encodeBinaryResult(['id'], [
    [[BinaryTag.I64, 1]],
  ]).subarray(0, 8)

  assert.throws(
    () => decodeResultPayload(payload),
    (err) => err instanceof RedDBError && err.code === 'PROTOCOL',
  )
})

test(
  'connect(red://) surfaces SELECT rows from a live server',
  { skip: process.env.RED_SMOKE === '1' ? false : 'set RED_SMOKE=1 to run live RedWire smoke' },
  async () => {
    const bin = redBinary()
    if (!bin) {
      throw new Error('RED_SMOKE=1 requires RED_BIN or target/debug/red')
    }

    const port = await pickFreePort()
    const dir = await mkdtemp(path.join(tmpdir(), 'reddb-js-redwire-'))
    const dataPath = path.join(dir, 'data.db')
    const server = spawn(bin, [
      'server',
      '--path',
      dataPath,
      '--bind',
      `127.0.0.1:${port}`,
    ], {
      stdio: 'ignore',
    })

    try {
      const db = await waitForRedwire(port)
      await db.query('CREATE TABLE js_redwire_rows (id INT, name TEXT)')
      await db.query("INSERT INTO js_redwire_rows (id, name) VALUES (1, 'Ada')")
      await db.query("INSERT INTO js_redwire_rows (id, name) VALUES (2, 'Grace')")

      const one = await db.query('SELECT 1')
      assert.deepEqual(one.columns, ['LIT:1'])
      assert.deepEqual(one.rows, [{ 'LIT:1': 1 }])

      const many = await db.query('SELECT id, name FROM js_redwire_rows ORDER BY id')
      assert.deepEqual(many.columns, ['id', 'name'])
      assert.deepEqual(many.rows, [
        { id: 1, name: 'Ada' },
        { id: 2, name: 'Grace' },
      ])

      await db.close()
    } finally {
      server.kill()
      await once(server, 'close').catch(() => {})
      await rm(dir, { recursive: true, force: true })
    }
  },
)

function encodeBinaryResult(columns, rows) {
  const enc = new TextEncoder()
  const bytes = []
  const pushU16 = (value) => {
    bytes.push(value & 0xff, (value >>> 8) & 0xff)
  }
  const pushU32 = (value) => {
    bytes.push(value & 0xff, (value >>> 8) & 0xff, (value >>> 16) & 0xff, (value >>> 24) & 0xff)
  }
  const pushBytes = (value) => {
    for (const byte of value) bytes.push(byte)
  }
  const pushI64 = (value) => {
    const buf = new Uint8Array(8)
    new DataView(buf.buffer).setBigInt64(0, BigInt(value), true)
    pushBytes(buf)
  }
  const pushU64 = (value) => {
    const buf = new Uint8Array(8)
    new DataView(buf.buffer).setBigUint64(0, BigInt(value), true)
    pushBytes(buf)
  }
  const pushF64 = (value) => {
    const buf = new Uint8Array(8)
    new DataView(buf.buffer).setFloat64(0, Number(value), true)
    pushBytes(buf)
  }
  const pushText = (value) => {
    const text = enc.encode(String(value))
    pushU32(text.length)
    pushBytes(text)
  }

  pushU16(columns.length)
  for (const column of columns) {
    const name = enc.encode(column)
    pushU16(name.length)
    pushBytes(name)
  }
  pushU32(rows.length)
  for (const row of rows) {
    assert.equal(row.length, columns.length)
    for (const [tag, value] of row) {
      bytes.push(tag)
      switch (tag) {
        case BinaryTag.Null:
          break
        case BinaryTag.I64:
          pushI64(value)
          break
        case BinaryTag.U64:
          pushU64(value)
          break
        case BinaryTag.F64:
          pushF64(value)
          break
        case BinaryTag.Text:
          pushText(value)
          break
        case BinaryTag.Bool:
          bytes.push(value ? 1 : 0)
          break
        default:
          throw new Error(`unknown test tag ${tag}`)
      }
    }
  }
  return Uint8Array.from(bytes)
}

function redBinary() {
  if (process.env.RED_BIN && existsSync(process.env.RED_BIN)) return process.env.RED_BIN
  const fallback = fileURLToPath(new URL('../../../target/debug/red', import.meta.url))
  return existsSync(fallback) ? fallback : null
}

async function waitForRedwire(port) {
  const deadline = Date.now() + 10_000
  let lastError = null
  while (Date.now() < deadline) {
    try {
      return await connect(`red://127.0.0.1:${port}`)
    } catch (err) {
      lastError = err
      await new Promise((resolve) => setTimeout(resolve, 50))
    }
  }
  throw lastError ?? new Error('server did not accept RedWire connections')
}

async function pickFreePort() {
  const listener = createServer()
  listener.listen(0, '127.0.0.1')
  await once(listener, 'listening')
  const { port } = listener.address()
  await new Promise((resolve) => listener.close(resolve))
  return port
}
