import test from 'node:test'
import assert from 'node:assert/strict'
import { RedDB } from '../src/core/reddb.js'
import { normalizeQueryParams } from '../src/core/serialization.js'
import { decodeFrame, encodeQueryWithParams, Features, MessageKind, RedWireClient } from '../src/redwire-core.js'

test('binary normalization preserves typed views and their exact wire values', () => {
  for (const value of [
    new Float32Array([0.25, -0, Infinity, -Infinity, NaN]),
    new Float64Array([1 / 3, -0, 1e300]),
    Uint8Array.of(0, 127, 255).subarray(1),
    Buffer.from([0, 127, 255]).subarray(1),
  ]) {
    const binary = normalizeQueryParams([[value]], true)
    const json = normalizeQueryParams([[value]])
    assert.equal(binary[0], value, 'binary transport retains the original view')
    assert.notEqual(json[0], value, 'JSON transport keeps its portable envelope')
    assert.deepEqual(encodeQueryWithParams('SELECT $1', binary), encodeQueryWithParams('SELECT $1', json))
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
