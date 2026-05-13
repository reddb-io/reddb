/**
 * Pure-codec tests for the RedWire `QueryWithParams` payload encoder.
 *
 * The wire-format expectations here are pinned against
 * `crates/reddb-wire/src/value.rs` and
 * `crates/reddb-wire/src/query_with_params.rs`. Any drift on either
 * side breaks the conformance contract for #357 (and eventually the
 * cross-driver fixtures in #373).
 */

import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

import {
  Features,
  ValueTag,
  MessageKind,
  encodeValue,
  encodeQueryWithParams,
} from '../src/redwire.js'

const HERE = dirname(fileURLToPath(import.meta.url))
const PARAM_FIXTURES = JSON.parse(readFileSync(
  resolve(HERE, '../../../crates/reddb-wire/tests/fixtures/params/manifest.json'),
  'utf8',
))

let passed = 0
let failed = 0

function test(name, fn) {
  try {
    fn()
    console.log(`  ok  ${name}`)
    passed++
  } catch (err) {
    console.error(`  FAIL ${name}\n        ${err.stack || err.message}`)
    failed++
  }
}

process.on('beforeExit', () => {
  console.log(`redwire params codec: ${passed} passed, ${failed} failed`)
  if (failed > 0) process.exitCode = 1
})

test('catalog: QueryWithParams discriminant pinned to 0x28', () => {
  assert.equal(MessageKind.QueryWithParams, 0x28)
})

test('catalog: FEATURE_PARAMS bit pinned to 0x01', () => {
  assert.equal(Features.PARAMS, 0x01)
})

test('catalog: ValueTag table pinned', () => {
  assert.deepEqual(ValueTag, Object.freeze({
    Null: 0x00, Bool: 0x01, Int: 0x02, Float: 0x03, Text: 0x04,
    Bytes: 0x05, Vector: 0x06, Json: 0x07, Timestamp: 0x08, Uuid: 0x09,
  }))
})

test('fixtures: manifest value encodings match JS RedWire codec', () => {
  for (const fixture of PARAM_FIXTURES.values) {
    const input = jsFixtureValue(fixture.name)
    assert.equal(hex(encodeValue(input)), fixture.redwire_hex, fixture.name)
  }
})

test('fixtures: manifest query encodings match JS RedWire codec', () => {
  for (const fixture of PARAM_FIXTURES.queries) {
    assert.equal(
      hex(encodeQueryWithParams(fixture.sql, fixture.params.map(jsFixtureValue))),
      fixture.redwire_hex,
      fixture.name,
    )
  }
})

test('encodeValue: null → tag-only', () => {
  assert.deepEqual(Array.from(encodeValue(null)), [0x00])
})

test('encodeValue: undefined coerces to null', () => {
  assert.deepEqual(Array.from(encodeValue(undefined)), [0x00])
})

test('encodeValue: bool', () => {
  assert.deepEqual(Array.from(encodeValue(true)), [0x01, 1])
  assert.deepEqual(Array.from(encodeValue(false)), [0x01, 0])
})

test('encodeValue: integer number → Int (i64 LE)', () => {
  // 1 → tag(0x02) + 0x01 0x00 ... 0x00 (8 bytes)
  assert.deepEqual(Array.from(encodeValue(1)), [0x02, 1, 0, 0, 0, 0, 0, 0, 0])
  // -1 → tag(0x02) + all 0xFF
  assert.deepEqual(Array.from(encodeValue(-1)), [0x02, ...new Array(8).fill(0xff)])
})

test('encodeValue: bigint always Int', () => {
  // 2^53 fits in i64; verify LE encoding.
  const enc = encodeValue(1n << 53n)
  assert.equal(enc[0], ValueTag.Int)
  const view = new DataView(enc.buffer, enc.byteOffset + 1, 8)
  assert.equal(view.getBigInt64(0, true), 1n << 53n)
})

test('encodeValue: non-integer number → Float (f64 LE)', () => {
  const enc = encodeValue(3.14)
  assert.equal(enc[0], ValueTag.Float)
  const view = new DataView(enc.buffer, enc.byteOffset + 1, 8)
  assert.equal(view.getFloat64(0, true), 3.14)
})

test('encodeValue: text utf-8 with u32 LE length', () => {
  const enc = encodeValue('héllo')
  assert.equal(enc[0], ValueTag.Text)
  const view = new DataView(enc.buffer, enc.byteOffset + 1, 4)
  const len = view.getUint32(0, true)
  assert.equal(len, 6) // "héllo" = 6 utf-8 bytes
  const body = new TextDecoder().decode(enc.subarray(5, 5 + len))
  assert.equal(body, 'héllo')
})

test('encodeValue: Uint8Array → Bytes', () => {
  const enc = encodeValue(new Uint8Array([0xde, 0xad, 0xbe, 0xef]))
  assert.deepEqual(Array.from(enc), [0x05, 4, 0, 0, 0, 0xde, 0xad, 0xbe, 0xef])
})

test('encodeValue: $bytes envelope → Bytes', () => {
  // base64 "SGVsbG8=" → "Hello"
  const enc = encodeValue({ $bytes: 'SGVsbG8=' })
  assert.deepEqual(Array.from(enc), [0x05, 5, 0, 0, 0, 0x48, 0x65, 0x6c, 0x6c, 0x6f])
})

test('encodeValue: $ts envelope → Timestamp (i64 LE seconds)', () => {
  const enc = encodeValue({ $ts: 1700000000 })
  assert.equal(enc[0], ValueTag.Timestamp)
  const view = new DataView(enc.buffer, enc.byteOffset + 1, 8)
  assert.equal(view.getBigInt64(0, true), 1700000000n)
})

test('encodeValue: $uuid envelope → Uuid (16 bytes)', () => {
  const enc = encodeValue({ $uuid: '00112233-4455-6677-8899-aabbccddeeff' })
  assert.equal(enc[0], ValueTag.Uuid)
  assert.deepEqual(
    Array.from(enc.subarray(1)),
    [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77,
      0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
  )
})

test('encodeValue: bad uuid rejects', () => {
  assert.throws(() => encodeValue({ $uuid: 'not-a-uuid' }), (e) => e.code === 'UUID_INVALID')
})

test('encodeValue: Float32Array → Vector (count + count*4 LE f32)', () => {
  const enc = encodeValue(new Float32Array([1, 2, -0.5]))
  assert.equal(enc[0], ValueTag.Vector)
  const view = new DataView(enc.buffer, enc.byteOffset + 1)
  assert.equal(view.getUint32(0, true), 3)
  assert.equal(view.getFloat32(4, true), 1)
  assert.equal(view.getFloat32(8, true), 2)
  assert.equal(view.getFloat32(12, true), -0.5)
})

test('encodeValue: number array → Vector (matches Float32Array path)', () => {
  const a = encodeValue([1, 2, 3])
  const b = encodeValue(Float32Array.from([1, 2, 3]))
  assert.deepEqual(Array.from(a), Array.from(b))
})

test('encodeValue: plain object → canonical Json', () => {
  // canonical keys sorted alphabetically: {"a":1,"b":2}
  const enc = encodeValue({ b: 2, a: 1 })
  assert.equal(enc[0], ValueTag.Json)
  const view = new DataView(enc.buffer, enc.byteOffset + 1, 4)
  const len = view.getUint32(0, true)
  const body = new TextDecoder().decode(enc.subarray(5, 5 + len))
  assert.equal(body, '{"a":1,"b":2}')
})

test('encodeValue: multi-key envelope falls through to Json', () => {
  // `{$bytes,foo}` is NOT a single-key envelope so it ships as Json.
  const enc = encodeValue({ $bytes: 'AA==', foo: 1 })
  assert.equal(enc[0], ValueTag.Json)
})

test('encodeValue: Symbol rejects', () => {
  assert.throws(() => encodeValue(Symbol('x')), (e) => e.code === 'UNSUPPORTED_PARAM')
})

test('encodeQueryWithParams: empty params', () => {
  const sql = 'SELECT 1'
  const enc = encodeQueryWithParams(sql, [])
  // u32 sql_len LE = 8, then 8 bytes utf-8, then u32 0 LE.
  assert.deepEqual(
    Array.from(enc.subarray(0, 4)),
    [8, 0, 0, 0],
  )
  assert.equal(new TextDecoder().decode(enc.subarray(4, 12)), sql)
  assert.deepEqual(Array.from(enc.subarray(12, 16)), [0, 0, 0, 0])
  assert.equal(enc.length, 16)
})

test('encodeQueryWithParams: mixed params encoded back-to-back', () => {
  const enc = encodeQueryWithParams('Q', [42, 'x', null])
  // header: sql_len=1, "Q", param_count=3
  assert.deepEqual(Array.from(enc.subarray(0, 4)), [1, 0, 0, 0])
  assert.equal(enc[4], 0x51) // 'Q'
  assert.deepEqual(Array.from(enc.subarray(5, 9)), [3, 0, 0, 0])
  // first value: Int(42)
  assert.equal(enc[9], ValueTag.Int)
  const intView = new DataView(enc.buffer, enc.byteOffset + 10, 8)
  assert.equal(intView.getBigInt64(0, true), 42n)
  // second value: Text("x") = tag(0x04) + u32(1) + 'x'
  assert.equal(enc[18], ValueTag.Text)
  assert.deepEqual(Array.from(enc.subarray(19, 23)), [1, 0, 0, 0])
  assert.equal(enc[23], 0x78) // 'x'
  // third value: Null
  assert.equal(enc[24], ValueTag.Null)
  assert.equal(enc.length, 25)
})

test('encodeQueryWithParams: param_count over limit rejects', () => {
  const arr = new Array(65_537).fill(null)
  assert.throws(
    () => encodeQueryWithParams('q', arr),
    (e) => e.code === 'PARAM_COUNT_OVER_LIMIT',
  )
})

test('encodeQueryWithParams: rejects non-array params', () => {
  assert.throws(() => encodeQueryWithParams('q', 'oops'), /TypeError/)
})

test('encodeQueryWithParams: rejects non-string sql', () => {
  assert.throws(() => encodeQueryWithParams(42, []), /TypeError/)
})

function hex(bytes) {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('')
}

function jsFixtureValue(name) {
  switch (name) {
    case 'null':
      return null
    case 'bool_true':
      return true
    case 'bool_false':
      return false
    case 'int_min':
      return -(1n << 63n)
    case 'int_max':
      return (1n << 63n) - 1n
    case 'int_42':
      return 42
    case 'float_nan':
      return Number.NaN
    case 'float_pos_inf':
      return Number.POSITIVE_INFINITY
    case 'float_neg_inf':
      return Number.NEGATIVE_INFINITY
    case 'float_subnormal_min':
      return Number.MIN_VALUE
    case 'text_unicode':
      return 'héllo'
    case 'text_x':
      return 'x'
    case 'bytes_empty':
      return new Uint8Array()
    case 'bytes_deadbeef':
      return new Uint8Array([0xde, 0xad, 0xbe, 0xef])
    case 'json_nested':
      return { z: [1, { deep: [true, false] }], a: null }
    case 'timestamp_zero':
      return { $ts: 0 }
    case 'timestamp_max':
      return { $ts: '9223372036854775807' }
    case 'uuid_001122':
      return { $uuid: '00112233-4455-6677-8899-aabbccddeeff' }
    case 'vector_empty':
      return []
    case 'vector_three':
      return [1, 2, -0.5]
    default:
      throw new Error(`unknown fixture ${name}`)
  }
}
