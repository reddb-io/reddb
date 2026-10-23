import test from 'node:test'
import assert from 'node:assert/strict'
import { RedDB } from '../src/core/reddb.js'
import { normalizeQueryParams } from '../src/core/serialization.js'
import { decodeFrame, encodeQueryWithParams, Features, MessageKind, RedWireClient } from '../src/redwire-core.js'

test('binary normalization preserves typed views and their exact wire values', () => {
  for (const value of [
    new Float32Array([0.25, -0, Infinity, -Infinity, NaN]),
    new Float64Array([1 / 3, -0, 1e300]),
    new Float32Array([9, 0.25, -0.5]).subarray(1),
    new Float32Array(0),
    new Float64Array(0),
    new Uint8Array(0),
    Uint8Array.of(0, 127, 255).subarray(1),
    Buffer.from([0, 127, 255]).subarray(1),
  ]) {
    const binary = normalizeQueryParams([[value]], true)
    const json = normalizeQueryParams([[value]])
    if (value.byteLength > 0) assert.equal(binary[0], value, 'binary transport retains the original view')
    else assert.deepEqual(binary, json)
    assert.notEqual(json[0], value, 'JSON transport keeps its portable envelope')
    assert.deepEqual(encodeQueryWithParams('SELECT $1', binary), encodeQueryWithParams('SELECT $1', json))
  }
})

test('detached typed views are rejected instead of becoming empty binary values', () => {
  for (const View of [Float32Array, Float64Array, Uint8Array]) {
    const value = new View(2)
    structuredClone(value.buffer, { transfer: [value.buffer] })
    assert.throws(() => normalizeQueryParams([[value]]), TypeError)
    assert.throws(() => normalizeQueryParams([[value]], true), TypeError)
  }
})

test('out-of-bounds resizable views retain the existing validation and encoding', (context) => {
  if (!ArrayBuffer.prototype.resize) { context.skip('Resizable ArrayBuffer unavailable'); return }
  for (const View of [Float32Array, Float64Array, Uint8Array]) {
    const buffer = new ArrayBuffer(16, { maxByteLength: 32 })
    const value = new View(buffer, 8, 1)
    buffer.resize(4)
    if (View === Uint8Array) {
      assert.deepEqual(normalizeQueryParams([[value]], true), normalizeQueryParams([[value]]))
    } else {
      assert.throws(() => normalizeQueryParams([[value]]), TypeError)
      assert.throws(() => normalizeQueryParams([[value]], true), TypeError)
    }
  }
})

test('binary normalization retains validation, parameter spreading and exact scalar envelopes', () => {
  const values = [new Date(0), 9007199254740993n, null, true, 'text', { x: 1 }, [1, 2]]
  assert.deepEqual(normalizeQueryParams([values], true), normalizeQueryParams([values]))
  assert.equal(normalizeQueryParams([], true), null)
  for (const value of [new Uint16Array(2), new Date(NaN), () => {}, Symbol('unsupported')]) {
    assert.throws(() => normalizeQueryParams([[value]], true), { code: 'UNSUPPORTED_PARAM' })
  }
})

test('RedDB query opts into binary parameters only for the RedWire transport', async () => {
  const vector = new Float32Array([0.5, 0.25])
  const bytes = Uint8Array.of(0, 255)
  for (const binaryQueryParams of [false, true]) {
    const calls = []
    const db = new RedDB({
      binaryQueryParams,
      call(method, params) { calls.push({ method, params }); return Promise.resolve({ rows: [] }) },
    })
    await db.query('SELECT $1, $2', [vector, bytes])
    assert.equal(calls[0].method, 'query')
    const params = calls[0].params.params
    if (binaryQueryParams) {
      assert.equal(params[0], vector)
      assert.equal(params[1], bytes)
    } else {
      assert.deepEqual(params, [[0.5, 0.25], { $bytes: 'AP8=' }])
    }
  }
  const transport = new RedWireClient({}, {}, '', 0)
  assert.equal(transport.binaryQueryParams, true)
})

test('mutating caller-owned typed views after query returns does not alter the sent request', async () => {
  const vector = new Float32Array([0.5, -0.25])
  const bytes = Uint8Array.of(0, 127, 255).subarray(1)
  const sql = 'SELECT $1, $2'
  const expected = encodeQueryWithParams(sql, normalizeQueryParams([[vector, bytes]]))
  const sent = []
  const transport = new RedWireClient(
    { write(frame, callback) { sent.push(frame); callback() } },
    { async next() { return { kind: MessageKind.Result, payload: new TextEncoder().encode('{"ok":true,"columns":[],"rows":[]}') } } },
    '', Features.PARAMS,
  )
  const pending = new RedDB(transport).query(sql, [vector, bytes])
  vector.fill(99)
  bytes.fill(99)
  await pending
  assert.equal(sent.length, 1)
  const frame = decodeFrame(sent[0])
  assert.equal(frame.kind, MessageKind.QueryWithParams)
  assert.deepEqual(frame.payload, expected)
})
