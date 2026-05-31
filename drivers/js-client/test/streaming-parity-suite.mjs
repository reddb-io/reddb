/**
 * Shared behavioural streaming suite (PRD #874 / #876).
 *
 * One suite, run against every streaming implementation that satisfies the
 * injected-streaming interface (`createSelectStream` / `createInputStream` +
 * the row reader/writer wrappers). It drives the implementation **directly
 * through that interface** against in-memory mock transport sessions — the
 * exact `{ [Symbol.asyncIterator], cancel }` read-session and
 * `{ write, close, cancel }` write-session contracts the wrappers consume —
 * so it asserts wrapper behaviour (frame classification, ordering,
 * cancellation, terminal envelope) without coupling to any one wire.
 *
 * Both `src/streaming.js` (Node) and `src/streaming-web.js` (Web) run this
 * same suite, so parity is *proven by shared execution*, not asserted.
 */

import { test } from 'node:test'
import assert from 'node:assert/strict'

import { RedDBError } from '../src/protocol.js'

/**
 * A mock read transport: `streamSelect()` returns a session whose iterator
 * replays `script` — an array of typed frames `{type,value}`, where an item
 * shaped `{ throw: err }` makes the iterator throw mid-stream (an error
 * frame). Records the cancel reason and the select opts.
 */
function mockReadClient(script) {
  const calls = { selectOpts: null, cancelReasons: [] }
  const client = {
    streamSelect(opts) {
      calls.selectOpts = opts
      let i = 0
      let cancelled = false
      return {
        [Symbol.asyncIterator]() {
          return {
            async next() {
              if (cancelled || i >= script.length) return { done: true, value: undefined }
              const item = script[i++]
              if (item && item.throw) throw item.throw
              return { done: false, value: item }
            },
          }
        },
        async cancel(reason) {
          cancelled = true
          calls.cancelReasons.push(reason)
        },
      }
    },
  }
  return { client, calls }
}

/** A mock read transport that dribbles rows forever until cancelled. */
function infiniteRowClient() {
  const calls = { cancelReasons: [] }
  const client = {
    streamSelect() {
      let i = 0
      let cancelled = false
      return {
        [Symbol.asyncIterator]() {
          return {
            async next() {
              if (cancelled) return { done: true, value: undefined }
              i += 1
              return { done: false, value: { type: 'row', value: { id: i } } }
            },
          }
        },
        async cancel(reason) {
          cancelled = true
          calls.cancelReasons.push(reason)
        },
      }
    },
  }
  return { client, calls }
}

/**
 * A mock write transport: `streamInput()` records every written row in order,
 * resolves `close()` with `envelope`, and records cancel reasons.
 */
function mockWriteClient(envelope) {
  const calls = { inputOpts: null, rows: [], closed: false, cancelReasons: [] }
  const client = {
    streamInput(opts) {
      calls.inputOpts = opts
      return {
        async write(row) {
          calls.rows.push(row)
        },
        async close() {
          calls.closed = true
          return envelope
        },
        async cancel(reason) {
          calls.cancelReasons.push(reason)
        },
      }
    },
  }
  return { client, calls }
}

/**
 * Register the behavioural suite for one streaming implementation.
 * @param {string} label e.g. 'node' / 'web'
 * @param {{ createSelectStream: Function, createInputStream: Function }} streaming
 */
export function runStreamingParitySuite(label, streaming) {
  test(`${label}: select yields typed frames in order, capturing descriptor/cursor/end`, async () => {
    const script = [
      { type: 'descriptor', value: { columns: [{ name: 'id' }, { name: 'name' }], schema_fingerprint: 'fp' } },
      { type: 'cursor', value: { token: 'tok-1', resumable: true } },
      { type: 'row', value: { id: 1, name: 'alice' } },
      { type: 'row', value: { id: 2, name: 'bob' } },
      { type: 'end', value: { row_count: 2 } },
    ]
    const { client, calls } = mockReadClient(script)
    const stream = streaming.createSelectStream(client, 'SELECT id, name FROM users')

    const rows = []
    for await (const row of stream) rows.push(row)

    assert.deepEqual(rows, [
      { id: 1, name: 'alice' },
      { id: 2, name: 'bob' },
    ])
    assert.equal(stream.descriptor.schema_fingerprint, 'fp')
    assert.deepEqual(stream.cursor, { token: 'tok-1', resumable: true })
    assert.equal(stream.endInfo.row_count, 2)
    // The wrapper forwards the query through the transport session contract.
    assert.equal(calls.selectOpts.sql, 'SELECT id, name FROM users')
  })

  test(`${label}: a mid-stream error frame rejects the iteration`, async () => {
    const boom = new RedDBError('query_error', 'boom mid-stream')
    const script = [
      { type: 'descriptor', value: { columns: [{ name: 'id' }] } },
      { type: 'row', value: { id: 1 } },
      { throw: boom },
    ]
    const { client } = mockReadClient(script)
    const stream = streaming.createSelectStream(client, 'SELECT id FROM users')

    const rows = []
    await assert.rejects(
      (async () => {
        for await (const row of stream) rows.push(row)
      })(),
      (err) => err instanceof RedDBError && err.code === 'query_error',
    )
    // A row consumed before the error is fine; the contract is error propagation.
    assert.ok(rows.length <= 1, `unexpected rows before error: ${JSON.stringify(rows)}`)
    if (rows.length === 1) assert.deepEqual(rows[0], { id: 1 })
  })

  test(`${label}: cancel() mid-stream aborts the transport session and rejects the iteration`, async () => {
    const { client, calls } = infiniteRowClient()
    const stream = streaming.createSelectStream(client, 'SELECT id FROM users')

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
    assert.deepEqual(calls.cancelReasons, ['done early'])
  })

  test(`${label}: select rejects an unsupported transport and an empty query`, () => {
    assert.throws(
      () => streaming.createSelectStream({}, 'SELECT 1'),
      (err) => err instanceof RedDBError && err.code === 'STREAMING_UNSUPPORTED',
    )
    const { client } = mockReadClient([])
    assert.throws(
      () => streaming.createSelectStream(client, '   '),
      (err) => err instanceof RedDBError && err.code === 'INVALID_STREAM_QUERY',
    )
  })

  test(`${label}: input delivers rows in order and resolves completion() with the terminal envelope`, async () => {
    const { client, calls } = mockWriteClient({ row_count: 2, committed_rid: 100, chunk_count: 2 })
    const sink = streaming.createInputStream(client, 'events')

    sink.write({ id: 1, name: 'a' })
    sink.write({ id: 2, name: 'b' })
    sink.end()
    const end = await sink.completion()

    assert.equal(end.row_count, 2)
    assert.equal(end.committed_rid, 100)
    assert.ok(calls.closed, 'close() must signal end-of-stream to the transport')
    assert.deepEqual(calls.rows, [
      { id: 1, name: 'a' },
      { id: 2, name: 'b' },
    ])
    assert.equal(calls.inputOpts.target, 'events')
  })

  test(`${label}: input cancel() aborts the session and rejects completion()`, async () => {
    const { client, calls } = mockWriteClient({ row_count: 0 })
    const sink = streaming.createInputStream(client, 'events')
    // A cancelled writable surfaces the failure on its 'error' event too (the
    // Node `Writable` raises it on `destroy(err)`); absorb it so the contract
    // under test — completion() rejecting — is what's asserted.
    sink.on('error', () => {})

    sink.write({ id: 1 })
    // Let the lazy transport session settle so cancel reaches it on both impls
    // (the Node `Writable` only forwards cancel to an already-resolved session).
    await new Promise((resolve) => setTimeout(resolve, 0))
    await sink.cancel('abort ingest')

    await assert.rejects(
      sink.completion(),
      (err) => err instanceof RedDBError && err.code === 'STREAM_CANCELLED',
    )
    assert.deepEqual(calls.cancelReasons, ['abort ingest'])
  })

  test(`${label}: input rejects an unsupported transport and an empty target`, () => {
    assert.throws(
      () => streaming.createInputStream({}, 'events'),
      (err) => err instanceof RedDBError && err.code === 'STREAMING_UNSUPPORTED',
    )
    const { client } = mockWriteClient({})
    assert.throws(
      () => streaming.createInputStream(client, '   '),
      (err) => err instanceof RedDBError && err.code === 'INVALID_STREAM_TARGET',
    )
  })
}
