/**
 * Web-native streaming surface for the JS driver (PRD #874 / #876).
 *
 * The browser/Web counterpart to `./streaming.js`. It exposes the **same**
 * streaming interface the Node implementation does — `createSelectStream`,
 * `createInputStream`, and the `RowReadable` / `RowWritable` row wrappers —
 * so it drops straight into the injected-streaming seam on the core `RedDB`
 * (`new RedDB(client, { createSelectStream, createInputStream })`). A browser
 * entry wires it in; this module imports **zero `node:` built-ins**.
 *
 * Where the Node version builds on `node:stream`'s `Readable` / `Writable`,
 * this one builds on the Web Streams primitives that already power the HTTP
 * transport's `fetch` sessions:
 *
 *   - `RowReadable`  — wraps a Web `ReadableStream` (object chunks) and is an
 *     `AsyncIterable<Row>`: `for await (const row of stream)` yields each row
 *     in order and exits cleanly on stream end. The descriptor and resumable
 *     cursor (when the transport surfaces them) are captured on
 *     `.descriptor` / `.cursor` and also fan out as `'descriptor'` /
 *     `'cursor'` events. A mid-stream error frame rejects the `for await`
 *     iteration with the transport's `RedDBError`.
 *   - `RowWritable` — wraps a Web `WritableStream`. `write(row)` pushes a row
 *     (backpressure flows through the returned promise / the writer's
 *     `ready`); `end()` signals end-of-stream; the server's terminal envelope
 *     resolves `.completion()`.
 *
 * Both expose the uniform `cancel(reason?)` the Node wrappers do: it aborts
 * the underlying transport session — which, over HTTP, calls
 * `AbortController.abort()` on the `fetch` — and rejects anything pending with
 * a `STREAM_CANCELLED` error.
 *
 * The transport session contracts consumed here are identical to the Node
 * ones (see `./streaming.js`): a read session is an async-iterable of typed
 * `{type,value}` frames plus `cancel(reason)`, and a write session is
 * `{ write(row), close(): Promise<EndEnvelope>, cancel(reason) }`.
 */

import { RedDBError } from './core/errors.js'
import { classifyNdjsonFrame } from './core/ndjson.js'

// Re-exported for parity with `./streaming.js`, so callers that reach for the
// NDJSON classifier from the streaming module keep working on either impl.
export { classifyNdjsonFrame }

function cancelError(reason) {
  const suffix = typeof reason === 'string' && reason.length > 0 ? `: ${reason}` : ''
  return new RedDBError('STREAM_CANCELLED', `stream cancelled${suffix}`)
}

function toError(err) {
  return err instanceof Error ? err : new RedDBError('STREAM_ERROR', String(err))
}

function deferred() {
  let resolve
  let reject
  const promise = new Promise((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

function abortReason(signal) {
  const reason = signal?.reason
  if (typeof reason === 'string') return reason
  if (reason && typeof reason.message === 'string') return reason.message
  return 'aborted'
}

/**
 * Attach a tiny `on` / `once` / `off` event surface to `target` and return an
 * internal `emit(event, ...args)`. Mirrors the events the Node `Readable` /
 * `Writable` raise (`descriptor` / `cursor` / `error` / `end` / `close`)
 * without dragging in `node:events`. Unlike Node's `EventEmitter`, an
 * unhandled `'error'` does not throw — the iteration rejection already carries
 * the failure.
 */
function attachEvents(target) {
  const listeners = new Map()
  target.on = (event, fn) => {
    let set = listeners.get(event)
    if (!set) {
      set = new Set()
      listeners.set(event, set)
    }
    set.add(fn)
    return target
  }
  target.off = (event, fn) => {
    listeners.get(event)?.delete(fn)
    return target
  }
  target.once = (event, fn) => {
    const wrapper = (...args) => {
      target.off(event, wrapper)
      fn(...args)
    }
    return target.on(event, wrapper)
  }
  return (event, ...args) => {
    const set = listeners.get(event)
    if (!set) return
    for (const fn of [...set]) {
      try {
        fn(...args)
      } catch {
        // a throwing listener must not derail the stream
      }
    }
  }
}

/**
 * `AsyncIterable<Row>` over a transport read session, backed by a Web
 * `ReadableStream`. Same surface as the Node `RowReadable`.
 */
export class RowReadable {
  /**
   * @param {Promise<object>} sessionPromise resolves to a read session.
   * @param {{ signal?: AbortSignal }} [opts]
   */
  constructor(sessionPromise, { signal } = {}) {
    this._sessionPromise = sessionPromise
    this._session = null
    this._iter = null
    this._ended = false
    this._cancelled = false
    this._cancelReason = undefined
    this._cancelDone = null
    this._controller = null
    /** Schema descriptor (HTTP NDJSON) once seen; null otherwise. */
    this.descriptor = null
    /** Resumable cursor control frame (#807) once seen; null otherwise. */
    this.cursor = null
    /** Terminal `end` envelope once the stream completes; null otherwise. */
    this.endInfo = null
    this._emit = attachEvents(this)

    this._stream = new ReadableStream({
      start: (controller) => {
        this._controller = controller
      },
      pull: (controller) => this._pull(controller),
      cancel: (reason) => this._forwardCancel(reason),
    })

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

  async _pull(controller) {
    if (this._cancelled) return
    let iter
    try {
      iter = await this._resolveIter()
    } catch (err) {
      if (this._cancelled) return
      const e = toError(err)
      this._emit('error', e)
      controller.error(e)
      return
    }
    // Loop until backpressure (one row enqueued) or completion. Control
    // frames (descriptor/cursor) are surfaced as events/properties and do not
    // count against the readable buffer, so we keep pulling past them.
    for (;;) {
      let result
      try {
        result = await iter.next()
      } catch (err) {
        if (this._cancelled) return
        const e = toError(err)
        this._emit('error', e)
        controller.error(e)
        return
      }
      if (this._cancelled) return
      const { value: frame, done } = result
      if (done) {
        this._ended = true
        this._emit('end', this.endInfo)
        controller.close()
        return
      }
      if (frame.type === 'descriptor') {
        this.descriptor = frame.value
        this._emit('descriptor', frame.value)
        continue
      }
      if (frame.type === 'cursor') {
        this.cursor = frame.value
        this._emit('cursor', frame.value)
        continue
      }
      if (frame.type === 'end') {
        this.endInfo = frame.value
        this._ended = true
        this._emit('end', frame.value)
        controller.close()
        return
      }
      // frame.type === 'row'
      controller.enqueue(frame.value)
      return
    }
  }

  _forwardCancel(reason) {
    return Promise.resolve(this._sessionPromise)
      .then((session) => (session && session.cancel ? session.cancel(reason) : undefined))
      .catch(() => {})
  }

  /**
   * `for await (const row of stream)` — single-consumer, like the Node
   * `Readable`. Yields rows in order; rejects on a mid-stream error frame or a
   * `cancel()`.
   */
  async *[Symbol.asyncIterator]() {
    const reader = this._stream.getReader()
    try {
      for (;;) {
        const { value, done } = await reader.read()
        if (done) return
        yield value
      }
    } finally {
      reader.releaseLock()
    }
  }

  /**
   * Terminate the stream. Sends a transport-level cancel and rejects any
   * pending `for await` iteration with a `STREAM_CANCELLED` error.
   * @param {string} [reason]
   * @returns {Promise<void>}
   */
  cancel(reason) {
    if (this._cancelled || this._ended) return this._cancelDone ?? Promise.resolve()
    this._cancelled = true
    this._cancelReason = reason
    // Error the readable so pending/future reads reject — `ReadableStream.cancel`
    // would merely resolve them with `{done:true}`, which is not the contract.
    try {
      this._controller?.error(cancelError(reason))
    } catch {
      // already closed/errored — fine.
    }
    this._emit('close')
    this._cancelDone = this._forwardCancel(reason)
    return this._cancelDone
  }
}

/**
 * `WritableStream`-backed sink over a transport write session. End-of-stream
 * is `end()`; the server's terminal envelope resolves `.completion()`. Same
 * surface as the Node `RowWritable`.
 */
export class RowWritable {
  /**
   * @param {Promise<object>} sessionPromise resolves to a write session.
   * @param {{ signal?: AbortSignal }} [opts]
   */
  constructor(sessionPromise, { signal } = {}) {
    this._sessionPromise = sessionPromise
    this._sessionResolved = null
    this._finished = false
    this._cancelled = false
    this._cancelReason = undefined
    this._cancelDone = null
    /** Terminal `end` envelope once the stream finishes; null otherwise. */
    this.endInfo = null
    this._emit = attachEvents(this)

    const completion = deferred()
    this._completionPromise = completion.promise
    this._resolveCompletion = completion.resolve
    this._rejectCompletion = completion.reject
    // Don't crash on an unobserved completion() — `'error'`/cancel carry it.
    this._completionPromise.catch(() => {})

    this._stream = new WritableStream({
      write: async (row) => {
        const session = await this._resolveSession()
        await session.write(row)
      },
      close: async () => {
        const session = await this._resolveSession()
        const end = await session.close()
        this.endInfo = end
        this._finished = true
        this._resolveCompletion(end)
        this._emit('finish', end)
      },
      abort: async () => {
        const session = await this._resolveSession().catch(() => null)
        if (session && session.cancel) {
          try {
            await session.cancel(this._cancelReason)
          } catch {
            // best-effort — the abort already tore the request down.
          }
        }
      },
    })
    this._writer = this._stream.getWriter()
    // Funnel any write/abort failure into completion() and an 'error' event.
    this._writer.closed.then(
      () => {},
      (err) => {
        const e = this._cancelled ? cancelError(this._cancelReason) : toError(err)
        this._rejectCompletion(e)
        if (!this._cancelled) this._emit('error', e)
      },
    )

    if (signal) {
      if (signal.aborted) {
        queueMicrotask(() => this.cancel(abortReason(signal)))
      } else {
        signal.addEventListener('abort', () => this.cancel(abortReason(signal)), { once: true })
      }
    }
  }

  async _resolveSession() {
    if (!this._sessionResolved) {
      this._sessionResolved = await this._sessionPromise
    }
    return this._sessionResolved
  }

  /**
   * Push a row. Returns a promise that resolves when the row is accepted;
   * backpressure flows through it (and the underlying writer's `ready`).
   * @param {object} row
   * @returns {Promise<void>}
   */
  write(row) {
    const p = this._writer.write(row)
    // Per-write failures surface via completion()/'error'; keep the returned
    // promise handled so an ignored write() can't trip an unhandled rejection.
    p.catch(() => {})
    return p
  }

  /**
   * Signal end-of-stream. The server's terminal envelope resolves
   * `.completion()`.
   * @returns {Promise<void>}
   */
  end() {
    const p = this._writer.close()
    p.catch(() => {})
    return p
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
   * `.completion()` with a `STREAM_CANCELLED` error and aborts the transport.
   * @param {string} [reason]
   * @returns {Promise<void>}
   */
  cancel(reason) {
    if (this._cancelled || this._finished) return this._cancelDone ?? Promise.resolve()
    this._cancelled = true
    this._cancelReason = reason
    this._rejectCompletion(cancelError(reason))
    this._emit('close')
    // abort() on the writable runs the abort algorithm above, which forwards
    // the cancel to the transport session.
    this._cancelDone = Promise.resolve(this._writer.abort(cancelError(reason))).catch(() => {})
    return this._cancelDone
  }
}

/**
 * Build a `RowReadable` from a connection's transport client. Identical
 * validation and session construction to the Node `createSelectStream`.
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
 * Build a `RowWritable` from a connection's transport client. Identical
 * validation and session construction to the Node `createInputStream`.
 * @param {object} client transport exposing `streamInput`.
 * @param {string} target table to ingest into.
 * @param {{ signal?: AbortSignal, columns?: string[] }} [opts]
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
