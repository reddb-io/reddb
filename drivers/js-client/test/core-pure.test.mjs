/**
 * Unit tests for the transport-agnostic core's pure functions, exercised in
 * isolation — no transport, no `connect()`, no `node:stream`. These guard the
 * extraction seam (#875): the wire-shaping logic must behave identically when
 * imported straight from the core barrel.
 */

import test from 'node:test'
import assert from 'node:assert/strict'

import {
  serializeParam,
  serializeJsonValue,
  normalizeExactNumbers,
  assertSupportedParam,
  normalizeQueryParams,
  bytesToBase64,
  isUuidString,
  mergeAuthFromUri,
  login,
  requireInsertId,
  requireInsertIds,
  classifyNdjsonFrame,
  splitLines,
  RedDBError,
} from '../src/core/index.js'

// ---------------------------------------------------------------------------
// serialization
// ---------------------------------------------------------------------------

test('serializeParam: primitives pass through untouched', () => {
  assert.equal(serializeParam(42), 42)
  assert.equal(serializeParam('hello'), 'hello')
  assert.equal(serializeParam(true), true)
  assert.equal(serializeParam(null), null)
  assert.equal(serializeParam(undefined), undefined)
})

test('serializeParam: bigint -> exact $int envelope', () => {
  assert.deepEqual(serializeParam(9007199254740993n), { $int: '9007199254740993' })
  assert.deepEqual(serializeParam(9223372036854775808n), { $uint: '9223372036854775808' })
})

test('serializeJsonValue: bigint body values use exact $int envelopes', () => {
  assert.deepEqual(
    serializeJsonValue({ id: 9007199254740993n, nested: [1n] }),
    { id: { $int: '9007199254740993' }, nested: [{ $int: '1' }] },
  )
})

test('normalizeExactNumbers: decodes exact envelopes and rejects superseded forms', () => {
  assert.deepEqual(
    normalizeExactNumbers({
      n: { $int: '9007199254740993' },
      u: { $uint: '9223372036854775808' },
      d: { $decimal: '3.14159265358979323846' },
    }),
    {
      n: 9007199254740993n,
      u: 9223372036854775808n,
      d: '3.14159265358979323846',
    },
  )
  assert.throws(
    () => normalizeExactNumbers({ n: { $number: '1' } }),
    (err) => err instanceof RedDBError && err.code === 'UNSUPPORTED_EXACT_NUMBER',
  )
})

test('serializeParam: Date → nanosecond $ts envelope', () => {
  const d = new Date('2020-01-01T00:00:00.000Z')
  assert.deepEqual(serializeParam(d), { $ts: String(BigInt(d.getTime()) * 1_000_000n) })
})

test('serializeParam: bytes → base64 $bytes envelope', () => {
  const bytes = new Uint8Array([1, 2, 3, 255])
  assert.deepEqual(serializeParam(bytes), { $bytes: bytesToBase64(bytes) })
  assert.equal(bytesToBase64(new Uint8Array([104, 105])), 'aGk=')
})

test('serializeParam: typed float arrays → plain number arrays', () => {
  assert.deepEqual(serializeParam(new Float32Array([1, 2, 3])), [1, 2, 3])
  assert.deepEqual(serializeParam(new Float64Array([0.5])), [0.5])
})

test('serializeParam: NaN / +Inf / -Inf → $float sentinels', () => {
  assert.deepEqual(serializeParam(NaN), { $float: 'NaN' })
  assert.deepEqual(serializeParam(Infinity), { $float: 'Infinity' })
  assert.deepEqual(serializeParam(-Infinity), { $float: '-Infinity' })
})

test('serializeParam: UUID strings → $uuid envelope (case-insensitive)', () => {
  // The driver's UUID pattern is 8-4-4-12 hex groups (case-insensitive).
  const uuid = '123e4567-e89b-12d3-426614174000'
  assert.deepEqual(serializeParam(uuid), { $uuid: uuid })
  assert.equal(isUuidString('123E4567-E89B-12D3-426614174000'), true)
  assert.equal(isUuidString('not-a-uuid'), false)
  // A non-matching string is left as-is.
  assert.equal(serializeParam('123e4567'), '123e4567')
})

test('assertSupportedParam: rejects invalid Date, mixed arrays, and exotic types', () => {
  assert.throws(() => assertSupportedParam(new Date('nope')), (e) =>
    e instanceof RedDBError && e.code === 'UNSUPPORTED_PARAM')
  assert.throws(() => assertSupportedParam([1, 'two']), (e) =>
    e instanceof RedDBError && e.code === 'UNSUPPORTED_PARAM')
  assert.throws(() => assertSupportedParam(() => {}), (e) =>
    e instanceof RedDBError && e.code === 'UNSUPPORTED_PARAM')
  // Plain objects and all-number arrays are accepted.
  assert.doesNotThrow(() => assertSupportedParam({ a: 1 }))
  assert.doesNotThrow(() => assertSupportedParam([1, 2, 3]))
})

test('normalizeQueryParams: empty → null, single-array spreads, varargs map', () => {
  assert.equal(normalizeQueryParams([]), null)
  assert.deepEqual(normalizeQueryParams([[1, 2, 3]]), [1, 2, 3])
  assert.deepEqual(normalizeQueryParams([1, 'a', true]), [1, 'a', true])
  // Each element still flows through serializeParam.
  const d = new Date(0)
  assert.deepEqual(normalizeQueryParams([d]), [{ $ts: '0' }])
})

// ---------------------------------------------------------------------------
// auth-merge
// ---------------------------------------------------------------------------

test('mergeAuthFromUri: URI-derived credentials with no options', () => {
  const parsed = { token: 'tok', username: 'u', password: 'p', loginUrl: 'http://x/login' }
  assert.deepEqual(mergeAuthFromUri(parsed, undefined), {
    token: 'tok', username: 'u', password: 'p', loginUrl: 'http://x/login',
  })
})

test('mergeAuthFromUri: apiKey on the URI falls back to token slot', () => {
  assert.equal(mergeAuthFromUri({ apiKey: 'ak-1' }, undefined).token, 'ak-1')
})

test('mergeAuthFromUri: options.auth overrides each field it sets', () => {
  const parsed = { token: 'uri-tok', username: 'uri-u' }
  const merged = mergeAuthFromUri(parsed, { username: 'opt-u', password: 'opt-p' })
  assert.equal(merged.token, 'uri-tok')   // untouched by options
  assert.equal(merged.username, 'opt-u')  // overridden
  assert.equal(merged.password, 'opt-p')
  // options.auth.apiKey wins the token slot.
  assert.equal(mergeAuthFromUri(parsed, { apiKey: 'opt-ak' }).token, 'opt-ak')
})

test('mergeAuthFromUri: rejects non-object auth and empty-string fields', () => {
  assert.throws(() => mergeAuthFromUri({}, 'nope'), TypeError)
  assert.throws(() => mergeAuthFromUri({}, { token: '' }), TypeError)
  assert.throws(() => mergeAuthFromUri({}, { username: '' }), TypeError)
})

test('login: validates inputs before any network call', async () => {
  await assert.rejects(() => login('ftp://x', { username: 'u', password: 'p' }), TypeError)
  await assert.rejects(() => login('http://x', { username: '', password: 'p' }), TypeError)
  await assert.rejects(() => login('http://x', { username: 'u', password: '' }), TypeError)
})

// ---------------------------------------------------------------------------
// insert-id normalization
// ---------------------------------------------------------------------------

test('requireInsertId: mirrors rid ↔ id both ways', () => {
  assert.deepEqual(requireInsertId({ rid: 7, affected: 1 }, 'insert'), { rid: 7, id: 7, affected: 1 })
  assert.deepEqual(requireInsertId({ id: 9, affected: 1 }, 'insert'), { rid: 9, id: 9, affected: 1 })
})

test('requireInsertId: missing both → ENGINE_TOO_OLD', () => {
  assert.throws(() => requireInsertId({ affected: 1 }, 'insert'), (e) =>
    e instanceof RedDBError && e.code === 'ENGINE_TOO_OLD')
  assert.throws(() => requireInsertId(null, 'insert'), (e) => e.code === 'ENGINE_TOO_OLD')
})

test('requireInsertIds: mirrors rids ↔ ids and enforces expected length', () => {
  assert.deepEqual(requireInsertIds({ rids: [1, 2] }, 2), { rids: [1, 2], ids: [1, 2] })
  assert.deepEqual(requireInsertIds({ ids: [3] }, 1), { rids: [3], ids: [3] })
  assert.throws(() => requireInsertIds({ rids: [1] }, 2), (e) =>
    e instanceof RedDBError && e.code === 'INVALID_RESPONSE')
  assert.throws(() => requireInsertIds({}, 1), (e) => e.code === 'ENGINE_TOO_OLD')
})

// ---------------------------------------------------------------------------
// NDJSON frame classification + line splitting
// ---------------------------------------------------------------------------

test('classifyNdjsonFrame: typed frames + blank-line null', () => {
  assert.equal(classifyNdjsonFrame('   '), null)
  assert.deepEqual(classifyNdjsonFrame('{"descriptor":{"cols":1}}'), { type: 'descriptor', value: { cols: 1 } })
  assert.deepEqual(classifyNdjsonFrame('{"cursor":"c1"}'), { type: 'cursor', value: 'c1' })
  assert.deepEqual(classifyNdjsonFrame('{"row":{"id":1}}'), { type: 'row', value: { id: 1 } })
  assert.deepEqual(classifyNdjsonFrame('{"end":{"affected":3}}'), { type: 'end', value: { affected: 3 } })
})

test('classifyNdjsonFrame: error frame throws with the carried code', () => {
  assert.throws(() => classifyNdjsonFrame('{"error":{"code":"BOOM","message":"nope"}}'), (e) =>
    e instanceof RedDBError && e.code === 'BOOM' && e.message === 'nope')
})

test('classifyNdjsonFrame: malformed JSON and unknown shapes throw STREAM_PROTOCOL', () => {
  assert.throws(() => classifyNdjsonFrame('{not json'), (e) =>
    e instanceof RedDBError && e.code === 'STREAM_PROTOCOL')
  assert.throws(() => classifyNdjsonFrame('{"mystery":1}'), (e) =>
    e instanceof RedDBError && e.code === 'STREAM_PROTOCOL')
})

test('splitLines: complete lines + trailing remainder', () => {
  assert.deepEqual(splitLines('a\nb\nc'), { lines: ['a', 'b'], rest: 'c' })
  assert.deepEqual(splitLines('a\nb\n'), { lines: ['a', 'b'], rest: '' })
  assert.deepEqual(splitLines('no newline'), { lines: [], rest: 'no newline' })
  assert.deepEqual(splitLines(''), { lines: [], rest: '' })
})
