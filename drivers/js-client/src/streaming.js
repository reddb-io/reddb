/**
 * Node-native streaming surface for the JS driver (PRD #759 / S11).
 *
 * Two transport-agnostic wrappers turn a low-level streaming *session*
 * — supplied by whichever transport the connection uses (HTTP NDJSON or
 * RedWire) — into idiomatic Node streams:
 *
 *   - `RowReadable`  — a `Readable` in object mode that also conforms to
 *     `AsyncIterable<Row>` (Readable already is one). Rows flow with
 *     natural backpressure via `read()` / `pause()` / `resume()`. The
 *     descriptor and the resumable cursor (when the transport surfaces
 *     them) arrive as `'descriptor'` / `'cursor'` events. A mid-stream
 *     `error` frame surfaces both as an `'error'` event and as a
 *     rejected `for await` iteration.
 *   - `RowWritable`  — a `Writable` in object mode. Backpressure flows
 *     through `write()`'s return value and `'drain'`. `end()` signals
 *     end-of-stream; the server's terminal envelope resolves the
 *     `.completion()` promise.
 *
 * Both expose a uniform `cancel(reason?)` that terminates the underlying
 * transport stream (a `StreamCancel` frame over RedWire, an
 * `AbortController.abort()` over HTTP) and rejects anything pending.
 *
 * The transport session contracts these wrappers consume:
 *
 *   read session (transport.streamSelect):
 *     { [Symbol.asyncIterator](): AsyncIterator<{type,value}>,
 *       cancel(reason): Promise<void> }
 *     where `type` is 'descriptor' | 'cursor' | 'row' | 'end'; the
 *     iterator throws a `RedDBError` when the server emits an error frame.
 *
 *   write session (transport.streamInput):
 *     { write(row): Promise<void>,      // resolves when accepted (backpressure)
 *       close(): Promise<EndEnvelope>,  // send terminal, await server end
 *       cancel(reason): Promise<void> }
 */

import { Readable, Writable, Transform } from 'node:stream'
import { RedDBError } from './protocol.js'
import { classifyNdjsonFrame } from './core/ndjson.js'

// `classifyNdjsonFrame` is a pure NDJSON helper that now lives in the
// transport-agnostic core; re-exported here so the historical
// `import { classifyNdjsonFrame } from './streaming.js'` path keeps working.
export { classifyNdjsonFrame }

function cancelError(reason) {
  const suffix = typeof reason === 'string' && reason.length > 0 ? `: ${reason}` : ''
  return new RedDBError('STREAM_CANCELLED', `stream cancelled${suffix}`)
}

/**
 * `Readable` (object mode) over a transport read session. Also an
 * `AsyncIterable<Row>` — `for await (const row of stream)` yields each
 * row and exits cleanly on stream end.
 */
export class RowReadable extends Readable {
  /**
   * @param {Promise<object>} sessionPromise resolves to a read session.
   * @param {{ signal?: AbortSignal }} [opts]
   */
  constructor(sessionPromise, { signal } = {}) {
    super({ objectMode: true })
    this._sessionPromise = sessionPromise
    this._session = null
    this._iter = null
    this._pumping = false
    this._ended = false
    this._cancelReason = undefined
    /** Schema descriptor (HTTP NDJSON) once seen; null otherwise. */
    this.descriptor = null
    /** Resumable cursor control frame (#807) once seen; null otherwise. */
    this.cursor = null
    /** Terminal `end` envelope once the stream completes; null otherwise. */
    this.endInfo = null
    if (signal) {
      if (signal.aborted) {
        queueMicrotask(() => this.cancel(abortReason(signal)))
      } else {
        signal.addEventListener('abort', () => this.cancel(abortReason(signal)), { once: true })
      }
    }
  }

  async _resolveIter() {
    if (this._iter) return this._iter
    this._session = await this._sessionPromise
    this._iter = this._session[Symbol.asyncIterator]()
    return this._iter
  }

  _read() {
    if (this._pumping) return
    this._pumping = true
    this._pump().then(
      () => {
        this._pumping = false
      },
      (err) => {
        this._pumping = false
        this.destroy(err instanceof Error ? err : new RedDBError('STREAM_ERROR', String(err)))
      },
    )
  }

  async _pump() {
    const iter = await this._resolveIter()
    // Loop until backpressure (push returned false) or completion. Non-row
    // control frames (descriptor/cursor) are surfaced as events and do not
    // count against the readable buffer, so we keep pulling after them.
    for (;;) {
      const { value: frame, done } = await iter.next()
      if (done) {
        this._ended = true
        this.push(null)
        return
      }
      if (frame.type === 'descriptor') {
        this.descriptor = frame.value
        this.emit('descriptor', frame.value)
        continue
      }
      if (frame.type === 'cursor') {
        this.cursor = frame.value
        this.emit('cursor', frame.value)
        continue
      }
      if (frame.type === 'end') {
        this.endInfo = frame.value
        this._ended = true
        this.push(null)
        return
      }
      // frame.type === 'row'
      if (!this.push(frame.value)) {
        return
      }
    }
  }

  _destroy(err, callback) {
    // A stream that ended on its own terminal frame needs no cancel — only
    // an early teardown (error or explicit cancel) signals the transport.
    const session = this._ended ? null : this._session
    const reason = this._cancelReason
    Promise.resolve(session && session.cancel ? session.cancel(reason) : undefined)
      .catch(() => {})
      .finally(() => callback(err))
  }

  /**
   * Terminate the stream. Sends a transport-level cancel and rejects any
   * pending `for await` iteration with a `STREAM_CANCELLED` error.
   * @param {string} [reason]
   * @returns {Promise<void>}
   */
  cancel(reason) {
    if (this.destroyed) return Promise.resolve()
    this._cancelReason = reason
    return new Promise((resolve) => {
      this.once('close', resolve)
      this.destroy(cancelError(reason))
    })
  }
}

/**
 * `Writable` (object mode) over a transport write session. End-of-stream
 * is `end()`; the server's terminal envelope resolves `.completion()`.
 */
export class RowWritable extends Writable {
  /**
   * @param {Promise<object>} sessionPromise resolves to a write session.
   * @param {{ signal?: AbortSignal }} [opts]
   */
  constructor(sessionPromise, { signal } = {}) {
    super({ objectMode: true })
    this._sessionPromise = sessionPromise
    this._session = null
    this._finished = false
    this._cancelReason = undefined
    this._completion = null
    this._completionPromise = new Promise((resolve, reject) => {
      this._completion = { resolve, reject }
    })
    // Don't crash the process if nobody attaches to completion() and the
    // stream errors — the 'error' event already carries the failure.
    this._completionPromise.catch(() => {})
    if (signal) {
      if (signal.aborted) {
        queueMicrotask(() => this.cancel(abortReason(signal)))
      } else {
        signal.addEventListener('abort', () => this.cancel(abortReason(signal)), { once: true })
      }
    }
  }

  async _resolveSession() {
    if (!this._session) this._session = await this._sessionPromise
    return this._session
  }

  _write(row, _enc, callback) {
    this._resolveSession()
      .then((session) => session.write(row))
      .then(() => callback(), (err) => callback(err))
  }

  _final(callback) {
    this._resolveSession()
      .then((session) => session.close())
      .then(
        (end) => {
          this._finished = true
          this._completion.resolve(end)
          callback()
        },
        (err) => {
          this._completion.reject(err)
          callback(err)
        },
      )
  }

  _destroy(err, callback) {
    if (err) this._completion.reject(err)
    const session = this._finished ? null : this._session
    const reason = this._cancelReason
    Promise.resolve(session && session.cancel ? session.cancel(reason) : undefined)
      .catch(() => {})
      .finally(() => callback(err))
  }

  /**
   * Resolves with the server's terminal `end` envelope once the stream
   * finishes successfully; rejects if the stream errors or is cancelled.
   * @returns {Promise<object>}
   */
  completion() {
    return this._completionPromise
  }

  /**
   * Terminate the stream without flushing the remaining rows. Rejects
   * `.completion()` with a `STREAM_CANCELLED` error.
   * @param {string} [reason]
   * @returns {Promise<void>}
   */
  cancel(reason) {
    if (this.destroyed) return Promise.resolve()
    this._cancelReason = reason
    return new Promise((resolve) => {
      this.once('close', resolve)
      this.destroy(cancelError(reason))
    })
  }
}

function abortReason(signal) {
  const reason = signal?.reason
  if (typeof reason === 'string') return reason
  if (reason && typeof reason.message === 'string') return reason.message
  return 'aborted'
}

/**
 * A `Transform` that splits an NDJSON byte/text stream into parsed row
 * objects, ready to pipe into `table.inputStream()`:
 *
 *   fs.createReadStream('rows.ndjson').pipe(splitNdjson()).pipe(table.inputStream())
 *
 * Each non-empty line is `JSON.parse`d; a `{ "row": {...} }` envelope is
 * unwrapped to its inner object so files written by the streaming reader
 * round-trip, while bare-object lines pass through untouched.
 * @returns {Transform}
 */
export function splitNdjson() {
  let buffer = ''
  const parseLine = (line, push, callback) => {
    const trimmed = line.trim()
    if (trimmed.length === 0) return true
    let parsed
    try {
      parsed = JSON.parse(trimmed)
    } catch (err) {
      callback(new RedDBError('NDJSON_PARSE_ERROR', `invalid NDJSON line: ${err.message}`))
      return false
    }
    const row = parsed && typeof parsed === 'object' && 'row' in parsed ? parsed.row : parsed
    push(row)
    return true
  }
  return new Transform({
    readableObjectMode: true,
    writableObjectMode: false,
    transform(chunk, _enc, callback) {
      buffer += chunk.toString('utf8')
      let nl
      while ((nl = buffer.indexOf('\n')) !== -1) {
        const line = buffer.slice(0, nl)
        buffer = buffer.slice(nl + 1)
        if (!parseLine(line, (row) => this.push(row), callback)) return
      }
      callback()
    },
    flush(callback) {
      if (parseLine(buffer, (row) => this.push(row), callback)) callback()
    },
  })
}

/**
 * Build a `RowReadable` from a connection's transport client.
 * @param {object} client transport exposing `streamSelect`.
 * @param {string} sql read-only SELECT to stream.
 * @param {{ signal?: AbortSignal, cursor?: string }} [opts]
 * @returns {RowReadable}
 */
export function createSelectStream(client, sql, opts = {}) {
  if (typeof client.streamSelect !== 'function') {
    throw new RedDBError(
      'STREAMING_UNSUPPORTED',
      'the active transport does not support streaming reads (use red:// or http(s)://)',
    )
  }
  if (opts.cursor == null && (typeof sql !== 'string' || sql.trim().length === 0)) {
    throw new RedDBError('INVALID_STREAM_QUERY', 'stream() requires a non-empty SQL string')
  }
  const sessionPromise = Promise.resolve().then(() =>
    client.streamSelect({ sql, cursor: opts.cursor, signal: opts.signal }),
  )
  return new RowReadable(sessionPromise, { signal: opts.signal })
}

/**
 * Build a `RowWritable` from a connection's transport client.
 * @param {object} client transport exposing `streamInput`.
 * @param {string} target table to ingest into.
 * @param {{ signal?: AbortSignal, columns?: string[] }} [opts] `columns`
 *   fixes the ingest column set; when omitted it is inferred from the
 *   keys of the first row written.
 * @returns {RowWritable}
 */
export function createInputStream(client, target, opts = {}) {
  if (typeof client.streamInput !== 'function') {
    throw new RedDBError(
      'STREAMING_UNSUPPORTED',
      'the active transport does not support streaming writes (use red:// or http(s)://)',
    )
  }
  if (typeof target !== 'string' || target.trim().length === 0) {
    throw new RedDBError('INVALID_STREAM_TARGET', 'inputStream() requires a non-empty target table')
  }
  const sessionPromise = Promise.resolve().then(() =>
    client.streamInput({
      target,
      columns: opts.columns,
      signal: opts.signal,
    }),
  )
  return new RowWritable(sessionPromise, { signal: opts.signal })
}
