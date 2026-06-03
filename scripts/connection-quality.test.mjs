/**
 * Tests for the connection-quality harness (scripts/connection-quality.mjs,
 * PRD #930 / issue #938, ADR 0036).
 *
 * The harness measures three transports — HTTP-over-TCP, RedWire-over-TCP and
 * RedWire-over-WSS — against a *live* server, so the live numbers are not
 * unit-testable here. What we lock down is the measurement logic that produces
 * those numbers, and in particular the RedWire-over-WSS path: we drive the
 * harness's measurement loops through the *real* WSS code path
 * (`connectRedwireWs`) over a mock binary WebSocket that speaks RedWire frames
 * with the native codec — the same mock strategy as
 * `drivers/js-client/test/redwire-ws.test.mjs`. A passing run proves the
 * harness drives a genuine RedWire-over-WSS client end to end without needing
 * a TLS edge.
 *
 * Run with: node --test scripts/connection-quality.test.mjs
 */

import { test } from 'node:test'
import assert from 'node:assert/strict'

import {
  parseArgs,
  normalizeWssUrl,
  summarize,
  measureRedwireSeq,
  measureRedwireConcurrency,
} from './connection-quality.mjs'
import { connectRedwireWs } from '../drivers/js-client/src/redwire-ws.js'
import { MessageKind, encodeFrame, decodeFrame } from '../drivers/js-client/src/redwire-core.js'

const MAGIC = 0xfe
const MINOR = 0x01

function jsonBytes(obj) {
  return new TextEncoder().encode(JSON.stringify(obj))
}
function asArrayBuffer(u8) {
  return u8.buffer.slice(u8.byteOffset, u8.byteOffset + u8.byteLength)
}

/**
 * Mock binary WebSocket with a scripted RedWire server behind it. Client bytes
 * are parsed with the native `decodeFrame`; responses are built with the native
 * `encodeFrame` and delivered back as `message` events, so the harness exercises
 * real framing over the WS transport in both directions.
 */
class MockRedWireWebSocket {
  constructor() {
    this.readyState = 1 // OPEN
    this.binaryType = 'blob'
    this._listeners = {}
    this._buf = new Uint8Array(0)
    this._gotPreamble = false
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
  _deliver(u8) {
    this._fire('message', { data: asArrayBuffer(u8) })
  }
  send(data) {
    this._feed(data instanceof Uint8Array ? data : new Uint8Array(data))
  }
  close() {
    if (this.readyState === 3) return
    this.readyState = 3
    this._fire('close', {})
  }
  _feed(chunk) {
    const merged = new Uint8Array(this._buf.length + chunk.length)
    merged.set(this._buf, 0)
    merged.set(chunk, this._buf.length)
    this._buf = merged
    if (!this._gotPreamble) {
      if (this._buf.length < 2) return
      this._buf = this._buf.subarray(2) // 0xFE magic + minor version
      this._gotPreamble = true
    }
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
      this._deliver(encodeFrame(MessageKind.HelloAck, corr, jsonBytes({ auth: 'anonymous', features: 0 })))
    } else if (frame.kind === MessageKind.AuthResponse) {
      this._deliver(encodeFrame(MessageKind.AuthOk, corr, jsonBytes({ session_id: 'mock', features: 0 })))
    } else if (frame.kind === MessageKind.Query || frame.kind === MessageKind.QueryBinary) {
      this._deliver(
        encodeFrame(
          MessageKind.Result,
          corr,
          jsonBytes({ ok: true, statement: 'SELECT', columns: ['n'], rows: [{ n: 1 }], affected: 0 }),
        ),
      )
    }
  }
}

async function connectMockWss() {
  const ws = new MockRedWireWebSocket()
  return await connectRedwireWs({
    url: 'wss://app.example.com:443/redwire',
    WebSocketImpl: function () {
      return ws
    },
  })
}

// ---- pure helpers -----------------------------------------------------------

test('normalizeWssUrl: bare host:port → wss://…/redwire, full URL passes through', () => {
  assert.equal(normalizeWssUrl('127.0.0.1:8443'), 'wss://127.0.0.1:8443/redwire')
  assert.equal(normalizeWssUrl('db.example.com:443'), 'wss://db.example.com:443/redwire')
  assert.equal(normalizeWssUrl('wss://h:9/redwire'), 'wss://h:9/redwire')
  assert.equal(normalizeWssUrl('ws://h:9/x'), 'ws://h:9/x')
})

test('parseArgs: accepts --ws/--wss and --ws-insecure', () => {
  const a = parseArgs(['node', 's', '--http', 'h:1', '--ws', 'wss://h:2/redwire', '--ws-insecure'])
  assert.equal(a.ws, 'wss://h:2/redwire')
  assert.equal(a.wsInsecure, true)
  const b = parseArgs(['node', 's', '--wss', 'h:2'])
  assert.equal(b.ws, 'h:2')
  assert.equal(b.wsInsecure, false)
})

test('summarize: percentiles, throughput and ordering are stable', () => {
  const row = summarize('x', [5, 1, 3, 2, 4], 1000, { note: 'n' })
  assert.equal(row.n, 5)
  assert.equal(row.p50, 3) // floor(0.5*5)=2 → sorted[2] = 3
  assert.equal(row.max, 5)
  assert.equal(row.mean, 3)
  assert.equal(row.throughput, 5) // 5 samples / 1000ms * 1000 = 5/s
  assert.equal(row.note, 'n')
})

// ---- RedWire-over-WSS measurement (real code path, mock transport) ----------

test('measureRedwireSeq: drives a real WSS client and records one sample per request', async () => {
  const client = await connectMockWss()
  const { samples, wall } = await measureRedwireSeq(client, 'SELECT 1', 25)
  assert.equal(samples.length, 25)
  assert.ok(samples.every((s) => Number.isFinite(s) && s >= 0))
  assert.ok(wall >= 0)
  const row = summarize('RedWire-over-WSS seq', samples, wall)
  assert.ok(Number.isFinite(row.p50) && Number.isFinite(row.p99))
  await client.close()
})

test('measureRedwireConcurrency: multiplexes in-flight queries over one WSS socket', async () => {
  const client = await connectMockWss()
  const { ok, errors, lat } = await measureRedwireConcurrency(client, 'SELECT 1', 8, 3)
  assert.equal(ok, 24) // 8 concurrency × 3 rounds, all succeed over one socket
  assert.equal(errors, 0)
  assert.equal(lat.length, 24)
  await client.close()
})
