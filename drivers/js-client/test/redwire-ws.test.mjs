/**
 * RedWire-over-binary-WebSocket transport (#937, ADR 0036).
 *
 * Proves the browser transport runs the *exact* RedWire handshake +
 * multiplexed query path over a binary WebSocket, with frame encode/decode
 * parity against the native codec — verified over a mock binary WebSocket,
 * no real socket. The mock server reuses the same `encodeFrame` /
 * `decodeFrame` the client uses, so a passing round-trip is itself the
 * parity assertion.
 *
 * Run with: node --test test/*.test.mjs
 */

import { test } from 'node:test'
import assert from 'node:assert/strict'

import { connectRedwireWs, WebSocketDuplex } from '../src/redwire-ws.js'
import { MessageKind, encodeFrame, decodeFrame } from '../src/redwire-core.js'

const MAGIC = 0xfe
const MINOR = 0x01

/** Wrap a Uint8Array as the `ArrayBuffer` a binary `WebSocket` delivers. */
function asArrayBuffer(u8) {
  return u8.buffer.slice(u8.byteOffset, u8.byteOffset + u8.byteLength)
}

/**
 * Mock binary WebSocket with a scripted RedWire server behind it. Client
 * bytes (`send`) are parsed with the native `decodeFrame`; responses are
 * built with the native `encodeFrame` and delivered back as `message`
 * events — so the test exercises real framing in both directions.
 */
class MockRedWireWebSocket {
  constructor() {
    this.readyState = 1 // OPEN
    this.binaryType = 'blob'
    this.sent = []
    this._listeners = {}
    // Inbound byte accumulator + preamble state for the server side.
    this._buf = new Uint8Array(0)
    this._gotPreamble = false
    // Open fires on the next microtask so `waitOpen` sees a real event.
    queueMicrotask(() => this._fire('open', {}))
  }

  addEventListener(type, cb) {
    ;(this._listeners[type] ??= []).push(cb)
  }

  removeEventListener(type, cb) {
    this._listeners[type] = (this._listeners[type] ?? []).filter((f) => f !== cb)
  }

  _fire(type, ev) {
    for (const cb of this._listeners[type] ?? []) cb(ev)
  }

  /** Deliver server→client bytes as a binary message. */
  _deliver(u8) {
    this._fire('message', { data: asArrayBuffer(u8) })
  }

  /** Client→server bytes. */
  send(data) {
    const u8 = data instanceof Uint8Array ? data : new Uint8Array(data)
    this.sent.push(u8)
    this._feed(u8)
  }

  close() {
    if (this.readyState === 3) return
    this.readyState = 3
    this._fire('close', {})
  }

  // --- scripted server -----------------------------------------------------

  _feed(chunk) {
    const merged = new Uint8Array(this._buf.length + chunk.length)
    merged.set(this._buf, 0)
    merged.set(chunk, this._buf.length)
    this._buf = merged

    if (!this._gotPreamble) {
      if (this._buf.length < 2) return
      assert.equal(this._buf[0], MAGIC, 'client must send the 0xFE magic')
      assert.equal(this._buf[1], MINOR, 'client must send the minor version')
      this._buf = this._buf.subarray(2)
      this._gotPreamble = true
    }

    // Drain whole frames as they complete.
    for (;;) {
      const frame = decodeFrame(this._buf)
      if (!frame) break
      this._buf = this._buf.subarray(frame.consumed)
      this._respond(frame)
    }
  }

  _respond(frame) {
    const corr = frame.correlationId
    if (frame.kind === MessageKind.Hello) {
      this._deliver(
        encodeFrame(MessageKind.HelloAck, corr, jsonBytes({ auth: 'anonymous', features: 0 })),
      )
    } else if (frame.kind === MessageKind.AuthResponse) {
      this._deliver(
        encodeFrame(MessageKind.AuthOk, corr, jsonBytes({ session_id: 'mock', features: 0 })),
      )
    } else if (
      frame.kind === MessageKind.Query
      || frame.kind === MessageKind.QueryBinary
    ) {
      const sql = new TextDecoder().decode(frame.payload)
      const rows = sql.includes('count') ? [{ n: 7 }] : [{ n: 42 }]
      this._deliver(
        encodeFrame(
          MessageKind.Result,
          corr,
          jsonBytes({ ok: true, statement: 'SELECT', columns: ['n'], rows, affected: 0 }),
        ),
      )
    }
  }
}

function jsonBytes(obj) {
  return new TextEncoder().encode(JSON.stringify(obj))
}

test('redwire-ws: connects over a mock binary WebSocket and round-trips a query', async () => {
  const ws = new MockRedWireWebSocket()
  const client = await connectRedwireWs({
    url: 'wss://app.example.com:443/redwire',
    WebSocketImpl: function () {
      return ws
    },
  })

  const result = await client.call('query', { sql: 'SELECT n FROM t' })
  assert.deepEqual(result.rows, [{ n: 42 }])

  // The first thing on the wire must be the exact native preamble.
  assert.equal(ws.sent[0][0], MAGIC)
  assert.equal(ws.sent[0][1], MINOR)

  await client.close()
})

test('redwire-ws: multiplexes several queries over one connection', async () => {
  const ws = new MockRedWireWebSocket()
  const client = await connectRedwireWs({
    url: 'wss://app.example.com:443/redwire',
    WebSocketImpl: function () {
      return ws
    },
  })

  const [a, b, c] = await Promise.all([
    client.call('query', { sql: 'SELECT n FROM t' }),
    client.call('query', { sql: 'SELECT count(*) AS n FROM t' }),
    client.call('query', { sql: 'SELECT n FROM t' }),
  ])
  assert.deepEqual(a.rows, [{ n: 42 }])
  assert.deepEqual(b.rows, [{ n: 7 }])
  assert.deepEqual(c.rows, [{ n: 42 }])

  await client.close()
})

test('redwire-ws: accepts a ws:// url for plaintext browser/dev endpoints', async () => {
  const ws = new MockRedWireWebSocket()
  const client = await connectRedwireWs({
    url: 'ws://localhost:80/redwire',
    WebSocketImpl: function () {
      return ws
    },
  })

  const result = await client.call('query', { sql: 'SELECT n FROM t' })
  assert.deepEqual(result.rows, [{ n: 42 }])

  await client.close()
})

test('redwire-ws: requires a ws:// or wss:// url', async () => {
  await assert.rejects(
    connectRedwireWs({ url: 'https://app.example.com/redwire', WebSocketImpl: () => ({}) }),
    (err) => err.code === 'WEBSOCKET_URL_REQUIRED',
  )
})

test('WebSocketDuplex: forwards writes to the socket byte-for-byte', () => {
  const sent = []
  const ws = {
    binaryType: 'blob',
    addEventListener() {},
    removeEventListener() {},
    send: (b) => sent.push(new Uint8Array(b)),
    close() {},
  }
  const duplex = new WebSocketDuplex(ws)
  const frame = encodeFrame(MessageKind.Query, 1n, new TextEncoder().encode('SELECT 1'))
  duplex.write(frame, () => {})
  assert.equal(sent.length, 1)
  assert.deepEqual(sent[0], frame)
})

test('WebSocketDuplex: surfaces binary messages as data events byte-for-byte', () => {
  const listeners = {}
  const ws = {
    binaryType: 'blob',
    addEventListener: (t, cb) => ((listeners[t] ??= []).push(cb)),
    removeEventListener() {},
    send() {},
    close() {},
  }
  const duplex = new WebSocketDuplex(ws)
  const seen = []
  duplex.on('data', (u8) => seen.push(u8))

  const frame = encodeFrame(MessageKind.Result, 9n, jsonBytes({ ok: true }))
  for (const cb of listeners.message) cb({ data: asArrayBuffer(frame) })

  assert.equal(seen.length, 1)
  assert.deepEqual(seen[0], frame)
  // And it decodes back to the same frame — full encode/decode parity.
  assert.equal(decodeFrame(seen[0]).kind, MessageKind.Result)
})
