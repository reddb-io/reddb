/**
 * RedWire client for Node / Bun / Deno.
 *
 * Speaks the binary TCP protocol from
 * `docs/adr/0001-redwire-tcp-protocol.md` directly — no spawn, no
 * HTTP. Mirrors `crates/reddb-client/src/redwire/` so the wire shape
 * stays in lockstep across drivers.
 *
 * Public surface:
 *   - `connectRedwire(opts)` → returns a `RedWireClient`
 *   - `RedWireClient.query(sql)` → JSON envelope
 *   - `RedWireClient.ping()`
 *   - `RedWireClient.close()`
 *
 * Auth methods this cut supports: `anonymous`, `bearer`. SCRAM /
 * mTLS / OAuth land in subsequent PRs.
 */

import { RedDBError } from './protocol.js'

const MAGIC = 0xfe
const SUPPORTED_VERSION = 0x01
const FRAME_HEADER_SIZE = 16
const MAX_FRAME_SIZE = 16 * 1024 * 1024
const KNOWN_FLAGS = 0b00000011

export const MessageKind = Object.freeze({
  Query: 0x01,
  Result: 0x02,
  Error: 0x03,
  BulkInsert: 0x04,
  BulkOk: 0x05,
  Hello: 0x10,
  HelloAck: 0x11,
  AuthRequest: 0x12,
  AuthResponse: 0x13,
  AuthOk: 0x14,
  AuthFail: 0x15,
  Bye: 0x16,
  Ping: 0x17,
  Pong: 0x18,
  Get: 0x19,
  Delete: 0x1A,
  DeleteOk: 0x1B,
  BulkInsertBinary: 0x06,
  QueryBinary: 0x07,
  BulkInsertPrevalidated: 0x08,
  QueryWithParams: 0x28,
  // Output/input streaming lifecycle (PRD #759). Mirrors
  // `reddb_wire::redwire::frame::MessageKind` so the JS streaming surface
  // talks the same multiplexed-stream vocabulary as the Rust server.
  RowDescription: 0x24,
  StreamEnd: 0x25,
  OpenStream: 0x29,
  OpenAck: 0x2A,
  StreamChunk: 0x2B,
  StreamError: 0x2C,
  StreamCancel: 0x2D,
})

export const Features = Object.freeze({ PARAMS: 0x0000_0001 })

export const ValueTag = Object.freeze({
  Null: 0x00, Bool: 0x01, Int: 0x02, Float: 0x03, Text: 0x04,
  Bytes: 0x05, Vector: 0x06, Json: 0x07, Timestamp: 0x08, Uuid: 0x09,
})

/**
 * Typed value tags for the binary fast path. Identical to the
 * engine-side `wire::protocol::VAL_*` table.
 */
export const BinaryTag = Object.freeze({
  Null: 0,
  I64: 1,
  F64: 2,
  Text: 3,
  Bool: 4,
  U64: 5,
})

const KIND_NAME = Object.fromEntries(
  Object.entries(MessageKind).map(([k, v]) => [v, k]),
)

export const Flags = Object.freeze({
  COMPRESSED: 0b00000001,
  MORE_FRAMES: 0b00000010,
})

/** zstd level for outbound compressed frames. Override via env. */
const ZSTD_LEVEL = (() => {
  const env = typeof process !== 'undefined' ? process.env?.RED_REDWIRE_ZSTD_LEVEL : null
  const n = env ? Number(env) : NaN
  return Number.isFinite(n) && n >= 1 && n <= 22 ? n : 1
})()

/**
 * Compress / decompress shim. zstd is **optional and injected** by the
 * host transport rather than imported here: the Node entry (`redwire.js`)
 * wires `node:zlib`; the browser entry leaves it null so compression is a
 * no-op. Keeping it injected is what lets this module stay free of any
 * `node:` specifier, so the exact same codec rides into the browser
 * bundle (#937, ADR 0036) past the portability guard. Peers still see the
 * COMPRESSED flag bit when offered; encode is a no-op until a provider is
 * set.
 */
let _zstdMod = null

/**
 * Install the zstd implementation (e.g. `node:zlib`). Pass `null` to
 * disable. Idempotent; the host transport calls this once at startup.
 */
export function setZstdProvider(mod) {
  _zstdMod = mod ?? null
}

/**
 * Run the RedWire handshake over an already-connected byte transport and
 * return a ready `RedWireClient`. The transport is any duplex exposing the
 * node-socket-shaped surface this codec consumes — `.on('data'|'error'|
 * 'end'|'close', cb)`, `.write(bytes, cb)`, `.end()`. The Node entry hands
 * a `node:net` / `node:tls` socket; the browser entry hands a binary
 * WebSocket adapter (#937, ADR 0036). The protocol logic is identical
 * across transports — that decoupling is the whole point.
 *
 * @param {object} socket Connected duplex byte transport.
 * @param {object} [opts]
 * @param {{ kind: 'anonymous' } | { kind: 'bearer', token: string }} [opts.auth]
 * @param {string} [opts.clientName]
 * @returns {Promise<RedWireClient>}
 */
export async function connectRedwireOverSocket(socket, opts = {}) {
  const auth = opts.auth ?? { kind: 'anonymous' }
  const reader = new FrameReader(socket)

  // Discriminator + minor-version byte.
  await writeAll(socket, Uint8Array.from([MAGIC, SUPPORTED_VERSION]))

  // Hello.
  const methods = auth.kind === 'bearer' ? ['bearer'] : ['anonymous', 'bearer']
  const helloPayload = jsonBytes({
    versions: [SUPPORTED_VERSION],
    auth_methods: methods,
    features: 0,
    client_name: opts.clientName ?? `reddb-js/0.2`,
  })
  await writeFrame(socket, MessageKind.Hello, 1n, helloPayload)

  const ack = await reader.next()
  if (ack.kind === MessageKind.AuthFail) {
    socket.end()
    const reason = jsonReason(ack.payload) ?? 'AuthFail at HelloAck'
    throw new RedDBError('AUTH_REFUSED', `redwire: ${reason}`)
  }
  if (ack.kind !== MessageKind.HelloAck) {
    socket.end()
    throw new RedDBError(
      'PROTOCOL',
      `expected HelloAck, got ${KIND_NAME[ack.kind] ?? ack.kind}`,
    )
  }
  const ackParsed = jsonOf(ack.payload)
  const chosenAuth = ackParsed?.auth
  if (typeof chosenAuth !== 'string') {
    socket.end()
    throw new RedDBError('PROTOCOL', 'HelloAck missing `auth` field')
  }

  // AuthResponse.
  let respPayload
  if (chosenAuth === 'anonymous') {
    respPayload = new Uint8Array()
  } else if (chosenAuth === 'bearer') {
    if (auth.kind !== 'bearer') {
      socket.end()
      throw new RedDBError(
        'AUTH_REFUSED',
        'server demanded bearer but no token was supplied',
      )
    }
    respPayload = jsonBytes({ token: auth.token })
  } else {
    socket.end()
    throw new RedDBError(
      'PROTOCOL',
      `server picked unsupported auth method: ${chosenAuth}`,
    )
  }
  await writeFrame(socket, MessageKind.AuthResponse, 2n, respPayload)

  const final = await reader.next()
  if (final.kind === MessageKind.AuthFail) {
    socket.end()
    const reason = jsonReason(final.payload) ?? 'auth refused'
    throw new RedDBError('AUTH_REFUSED', reason)
  }
  if (final.kind !== MessageKind.AuthOk) {
    socket.end()
    throw new RedDBError(
      'PROTOCOL',
      `expected AuthOk, got ${KIND_NAME[final.kind] ?? final.kind}`,
    )
  }
  const session = jsonOf(final.payload) ?? {}
  const features = numberOr(session.features, numberOr(ackParsed?.features, 0))

  return new RedWireClient(socket, reader, session, features)
}

function numberOr(v, fallback) {
  return typeof v === 'number' && Number.isFinite(v) ? v : fallback
}

/**
 * Returned by `connectRedwire`. Methods map 1:1 to RedWire frame
 * kinds. Reuses the same `RedDB`-shaped envelope as the other
 * transports so the surface above this is uniform.
 */
export class RedWireClient {
  constructor(socket, reader, session, serverFeatures = 0) {
    this.socket = socket
    this.reader = reader
    this.session = session
    this.serverFeatures = serverFeatures >>> 0
    this.nextCorr = 1n
    this.nextStream = 1
    this.closed = false
  }

  /** Raw advertised server feature bitmask. */
  features() {
    return this.serverFeatures
  }

  /** True when server advertised `FEATURE_PARAMS` (#357). */
  supportsParams() {
    return (this.serverFeatures & Features.PARAMS) === Features.PARAMS
  }

  async call(method, params = {}) {
    if (method === 'query') return this.#query(params.sql ?? '', params.params)
    if (method === 'insert') return this.#insert({ collection: params.collection, payload: params.payload })
    if (method === 'bulk_insert') return this.#insert({ collection: params.collection, payloads: params.payloads })
    if (method === 'bulk_insert_binary') {
      return this.bulkInsertBinary(params.collection, params.columns, params.rows)
    }
    if (method === 'get') return this.#getOrDelete(MessageKind.Get, MessageKind.Result, params)
    if (method === 'delete') return this.#getOrDelete(MessageKind.Delete, MessageKind.DeleteOk, params)
    if (method === 'health' || method === 'version') return this.#ping()
    throw new RedDBError(
      'UNKNOWN_METHOD',
      `RedWire transport doesn't bridge '${method}' yet`,
    )
  }

  /**
   * Bulk-insert via the binary fast path (frame kind 0x06).
   * Same hot-loop perf as the engine's `MSG_BULK_INSERT_BINARY`
   * stress tests. Each row is an array of `[tag, value]` pairs
   * matching the column order; tag values come from `BinaryTag`.
   *
   * Example:
   *   client.bulkInsertBinary('users', ['name', 'age'], [
   *     [[BinaryTag.Text, 'alice'], [BinaryTag.I64, 30n]],
   *     [[BinaryTag.Text, 'bob'],   [BinaryTag.I64, 25n]],
   *   ])
   */
  async bulkInsertBinary(collection, columns, rows) {
    if (!Array.isArray(columns) || !Array.isArray(rows)) {
      throw new TypeError('bulkInsertBinary: columns and rows must be arrays')
    }
    const buf = encodeBinaryBulk(collection, columns, rows)
    const corr = this.#corr()
    await writeFrame(this.socket, MessageKind.BulkInsertBinary, corr, buf)
    const resp = await this.reader.next()
    if (resp.kind === MessageKind.BulkOk) {
      // v1 BulkOk body is an 8-byte little-endian count.
      if (resp.payload.length < 8) {
        throw new RedDBError('PROTOCOL', 'BulkOk truncated: expected 8-byte count')
      }
      const view = new DataView(
        resp.payload.buffer,
        resp.payload.byteOffset,
        resp.payload.byteLength,
      )
      return Number(view.getBigUint64(0, true))
    }
    if (resp.kind === MessageKind.Error) {
      throw new RedDBError('ENGINE', new TextDecoder().decode(resp.payload))
    }
    throw new RedDBError(
      'PROTOCOL',
      `expected BulkOk/Error, got ${KIND_NAME[resp.kind] ?? resp.kind}`,
    )
  }

  async #getOrDelete(reqKind, okKind, params) {
    const corr = this.#corr()
    const payload = jsonBytes({ collection: params.collection, id: String(params.id) })
    await writeFrame(this.socket, reqKind, corr, payload)
    const resp = await this.reader.next()
    if (resp.kind === okKind) return jsonOf(resp.payload) ?? {}
    if (resp.kind === MessageKind.Error) {
      throw new RedDBError('ENGINE', new TextDecoder().decode(resp.payload))
    }
    throw new RedDBError(
      'PROTOCOL',
      `expected ${KIND_NAME[okKind]}/Error, got ${KIND_NAME[resp.kind] ?? resp.kind}`,
    )
  }

  async #insert(body) {
    const corr = this.#corr()
    const payload = jsonBytes(body)
    await writeFrame(this.socket, MessageKind.BulkInsert, corr, payload)
    const resp = await this.reader.next()
    if (resp.kind === MessageKind.BulkOk) {
      return jsonOf(resp.payload) ?? { affected: 0 }
    }
    if (resp.kind === MessageKind.Error) {
      throw new RedDBError('ENGINE', new TextDecoder().decode(resp.payload))
    }
    throw new RedDBError(
      'PROTOCOL',
      `expected BulkOk/Error, got ${KIND_NAME[resp.kind] ?? resp.kind}`,
    )
  }

  async #query(sql, params) {
    const corr = this.#corr()
    const hasParams = Array.isArray(params) && params.length > 0
    let kind
    let payload
    if (hasParams) {
      if (!this.supportsParams()) {
        throw new RedDBError(
          'PARAMS_UNSUPPORTED',
          'server did not advertise FEATURE_PARAMS — upgrade the server '
            + 'to one that supports parameterized queries.',
        )
      }
      kind = MessageKind.QueryWithParams
      payload = encodeQueryWithParams(sql, params)
    } else {
      kind = isSelectQuery(sql) ? MessageKind.QueryBinary : MessageKind.Query
      payload = new TextEncoder().encode(sql)
    }
    await writeFrame(this.socket, kind, corr, payload)
    const resp = await this.reader.next()
    if (resp.kind === MessageKind.Result) {
      return decodeResultPayload(resp.payload)
    }
    if (resp.kind === MessageKind.Error) {
      throw new RedDBError(
        'ENGINE',
        new TextDecoder().decode(resp.payload),
      )
    }
    throw new RedDBError(
      'PROTOCOL',
      `expected Result/Error, got ${KIND_NAME[resp.kind] ?? resp.kind}`,
    )
  }

  async #ping() {
    const corr = this.#corr()
    await writeFrame(this.socket, MessageKind.Ping, corr, new Uint8Array())
    const resp = await this.reader.next()
    if (resp.kind !== MessageKind.Pong) {
      throw new RedDBError(
        'PROTOCOL',
        `expected Pong, got ${KIND_NAME[resp.kind] ?? resp.kind}`,
      )
    }
    return { ok: true }
  }

  /**
   * Open a streaming read over RedWire. Sends `OpenStream` and returns an
   * async iterable of typed frames (see streaming.js) plus a
   * `cancel(reason)` that emits a `StreamCancel` for this stream_id. The
   * `OpenAck` is consumed internally; rows arrive as `StreamChunk`s and
   * the stream closes on `StreamEnd`. A `StreamError` rejects iteration.
   *
   * @param {{ sql?: string, cursor?: string }} opts
   */
  async streamSelect({ sql, cursor } = {}) {
    if (cursor != null) {
      throw new RedDBError(
        'STREAM_CURSOR_UNSUPPORTED',
        'resumable cursors are only available over the HTTP transport in this release',
      )
    }
    const streamId = this.#stream()
    const corr = this.#corr()
    await this.#writeStreamFrame(MessageKind.OpenStream, corr, jsonBytes({ sql }), streamId)

    const reader = this.reader
    const client = this
    return {
      async *[Symbol.asyncIterator]() {
        for (;;) {
          const resp = await reader.next()
          if (resp.streamId !== 0 && resp.streamId !== streamId) {
            continue
          }
          if (resp.kind === MessageKind.OpenAck) {
            continue
          }
          if (resp.kind === MessageKind.StreamChunk) {
            const chunk = jsonOf(resp.payload) ?? {}
            const rows = Array.isArray(chunk.rows) ? chunk.rows : []
            for (const row of rows) {
              yield { type: 'row', value: row }
            }
            continue
          }
          if (resp.kind === MessageKind.StreamEnd) {
            const end = jsonOf(resp.payload) ?? {}
            yield { type: 'end', value: end.stats ?? end }
            return
          }
          if (resp.kind === MessageKind.StreamError || resp.kind === MessageKind.Error) {
            const err = jsonOf(resp.payload) ?? {}
            throw new RedDBError(
              err.code || 'STREAM_ERROR',
              err.message || new TextDecoder().decode(resp.payload),
              err,
            )
          }
          throw new RedDBError(
            'STREAM_PROTOCOL',
            `unexpected frame in stream: ${KIND_NAME[resp.kind] ?? resp.kind}`,
          )
        }
      },
      async cancel(reason) {
        await client.#cancelStream(streamId, reason)
      },
    }
  }

  /**
   * Open a streaming write over RedWire. The `OpenStream {direction:"in"}`
   * frame is sent on the first `write()` (so columns can be inferred from
   * the first row); each row is shipped as a one-row `StreamChunk` and the
   * terminal chunk closes the input phase. `close()` resolves with the
   * server's `StreamEnd` stats.
   *
   * @param {{ target: string, columns?: string[] }} opts
   */
  async streamInput({ target, columns } = {}) {
    const streamId = this.#stream()
    const corr = this.#corr()
    const client = this
    let opened = false
    let seq = 0
    let cols = Array.isArray(columns) && columns.length > 0 ? columns.slice() : null

    const ensureOpen = async (row) => {
      if (opened) return
      if (!cols) {
        cols = row && typeof row === 'object' ? Object.keys(row) : null
      }
      if (!cols || cols.length === 0) {
        throw new RedDBError(
          'INVALID_STREAM_COLUMNS',
          'inputStream() needs a non-empty column set — pass { columns } or write at least one object row',
        )
      }
      await client.#writeStreamFrame(
        MessageKind.OpenStream,
        corr,
        jsonBytes({ direction: 'in', target, columns: cols }),
        streamId,
      )
      const ack = await client.reader.next()
      if (ack.kind === MessageKind.StreamError || ack.kind === MessageKind.Error) {
        const err = jsonOf(ack.payload) ?? {}
        throw new RedDBError(err.code || 'STREAM_ERROR', err.message || 'input stream refused', err)
      }
      if (ack.kind !== MessageKind.OpenAck) {
        throw new RedDBError(
          'STREAM_PROTOCOL',
          `expected OpenAck, got ${KIND_NAME[ack.kind] ?? ack.kind}`,
        )
      }
      opened = true
    }

    return {
      async write(row) {
        await ensureOpen(row)
        await client.#writeStreamFrame(
          MessageKind.StreamChunk,
          corr,
          jsonBytes({ seq: seq++, rows: [row], terminal: false }),
          streamId,
        )
      },
      async close() {
        await ensureOpen(null)
        await client.#writeStreamFrame(
          MessageKind.StreamChunk,
          corr,
          jsonBytes({ seq: seq++, rows: [], terminal: true }),
          streamId,
        )
        const end = await client.reader.next()
        if (end.kind === MessageKind.StreamError || end.kind === MessageKind.Error) {
          const err = jsonOf(end.payload) ?? {}
          throw new RedDBError(err.code || 'STREAM_ERROR', err.message || 'input stream failed', err)
        }
        if (end.kind !== MessageKind.StreamEnd) {
          throw new RedDBError(
            'STREAM_PROTOCOL',
            `expected StreamEnd, got ${KIND_NAME[end.kind] ?? end.kind}`,
          )
        }
        const parsed = jsonOf(end.payload) ?? {}
        return parsed.stats ?? parsed
      },
      async cancel(reason) {
        await client.#cancelStream(streamId, reason)
      },
    }
  }

  async #cancelStream(streamId, reason) {
    if (this.closed) return
    const payload = typeof reason === 'string' && reason.length > 0
      ? jsonBytes({ reason })
      : new Uint8Array()
    try {
      await this.#writeStreamFrame(MessageKind.StreamCancel, this.#corr(), payload, streamId)
    } catch {
      // best-effort — the socket may already be torn down.
    }
  }

  #writeStreamFrame(kind, corr, payload, streamId) {
    const buf = encodeFrame(kind, corr, payload, 0, streamId)
    return writeAll(this.socket, buf)
  }

  async close() {
    if (this.closed) return
    this.closed = true
    try {
      const corr = this.#corr()
      await writeFrame(this.socket, MessageKind.Bye, corr, new Uint8Array())
    } catch {
      // best-effort
    }
    this.socket.end()
  }

  #corr() {
    const c = this.nextCorr
    this.nextCorr = this.nextCorr + 1n
    return c
  }

  #stream() {
    const id = this.nextStream
    // stream_id 0 is reserved for handshake/lifecycle frames; wrap past it.
    this.nextStream = this.nextStream >= 0xffff ? 1 : this.nextStream + 1
    return id
  }
}

// ---------------------------------------------------------------------------
// Framing helpers
// ---------------------------------------------------------------------------

export function encodeFrame(kind, correlationId, payload, flags = 0, streamId = 0) {
  if (!(payload instanceof Uint8Array)) {
    payload = new Uint8Array(payload)
  }
  let onWire = payload
  let outFlags = flags & KNOWN_FLAGS
  // We compress synchronously when the flag is set AND the
  // runtime ships native zstd. Async flag-flip happens at
  // session level (see RedWireClient construction); per-frame
  // call here is a fast Buffer roundtrip.
  if (outFlags & Flags.COMPRESSED && _zstdMod && typeof _zstdMod.zstdCompressSync === 'function') {
    try {
      const compressed = _zstdMod.zstdCompressSync(payload, {
        params: { [_zstdMod.constants?.ZSTD_c_compressionLevel ?? 100]: ZSTD_LEVEL },
      })
      onWire = compressed instanceof Uint8Array ? compressed : new Uint8Array(compressed)
    } catch {
      // Fallback: ship plaintext, drop the flag so the peer
      // doesn't try to decompress.
      outFlags &= ~Flags.COMPRESSED
    }
  }
  const length = FRAME_HEADER_SIZE + onWire.length
  if (length > MAX_FRAME_SIZE) {
    throw new RedDBError('FRAME_TOO_LARGE', `frame ${length} > ${MAX_FRAME_SIZE}`)
  }
  const buf = new Uint8Array(length)
  const view = new DataView(buf.buffer)
  view.setUint32(0, length, true)
  buf[4] = kind
  buf[5] = outFlags
  view.setUint16(6, streamId, true)
  view.setBigUint64(8, BigInt(correlationId), true)
  buf.set(onWire, FRAME_HEADER_SIZE)
  return buf
}

function writeFrame(socket, kind, correlationId, payload) {
  const buf = encodeFrame(kind, correlationId, payload)
  return writeAll(socket, buf)
}

export function decodeFrame(buf) {
  if (buf.length < FRAME_HEADER_SIZE) return null
  const view = new DataView(buf.buffer, buf.byteOffset, buf.byteLength)
  const length = view.getUint32(0, true)
  if (length < FRAME_HEADER_SIZE || length > MAX_FRAME_SIZE) {
    throw new RedDBError('FRAME_INVALID_LENGTH', `length=${length}`)
  }
  if (buf.length < length) return null
  const kind = buf[4]
  const flags = buf[5]
  if (flags & ~KNOWN_FLAGS) {
    throw new RedDBError('FRAME_UNKNOWN_FLAGS', `flags=0x${flags.toString(16)}`)
  }
  const streamId = view.getUint16(6, true)
  const correlationId = view.getBigUint64(8, true)
  let payload = buf.slice(FRAME_HEADER_SIZE, length)
  if (flags & Flags.COMPRESSED) {
    if (!_zstdMod || typeof _zstdMod.zstdDecompressSync !== 'function') {
      throw new RedDBError(
        'COMPRESSED_BUT_NO_ZSTD',
        'incoming frame has COMPRESSED flag but runtime has no zstd support — upgrade Node >= 22',
      )
    }
    try {
      const plain = _zstdMod.zstdDecompressSync(payload)
      payload = plain instanceof Uint8Array ? plain : new Uint8Array(plain)
    } catch (err) {
      throw new RedDBError('FRAME_DECOMPRESS_FAILED', err.message)
    }
  }
  return { kind, flags, streamId, correlationId, payload, consumed: length }
}

/**
 * Buffered frame reader — TCP delivers byte streams, frames may
 * cross or share `data` events. Maintains a rolling accumulator
 * and yields one frame per `next()` call.
 */
class FrameReader {
  constructor(socket) {
    this.chunks = []
    this.totalLen = 0
    this.waiters = []
    this.error = null
    this.eof = false
    socket.on('data', (chunk) => {
      // chunk is Buffer (Node) or Uint8Array (Bun/Deno)
      const u8 = chunk instanceof Uint8Array ? chunk : new Uint8Array(chunk)
      this.chunks.push(u8)
      this.totalLen += u8.length
      this.#tryDeliver()
    })
    socket.on('error', (err) => {
      this.error = err
      this.#flushWaiters()
    })
    socket.on('end', () => {
      this.eof = true
      this.#flushWaiters()
    })
    socket.on('close', () => {
      this.eof = true
      this.#flushWaiters()
    })
  }

  next() {
    if (this.error) return Promise.reject(this.error)
    return new Promise((resolve, reject) => {
      this.waiters.push({ resolve, reject })
      this.#tryDeliver()
    })
  }

  #tryDeliver() {
    while (this.waiters.length > 0 && this.totalLen > 0) {
      const flat = this.#flatten()
      let frame
      try {
        frame = decodeFrame(flat)
      } catch (err) {
        const w = this.waiters.shift()
        w.reject(err)
        return
      }
      if (frame == null) {
        // Need more bytes — put the flattened buffer back as a
        // single chunk so we don't keep flattening repeatedly.
        this.chunks = [flat]
        return
      }
      this.chunks = [flat.subarray(frame.consumed)]
      this.totalLen = this.chunks[0].length
      const w = this.waiters.shift()
      w.resolve(frame)
    }
    if (this.eof && this.waiters.length > 0 && this.totalLen === 0) {
      const err = this.error ?? new RedDBError('CONNECTION_CLOSED', 'redwire: connection closed')
      while (this.waiters.length > 0) {
        this.waiters.shift().reject(err)
      }
    }
  }

  #flushWaiters() {
    if (this.waiters.length === 0) return
    if (this.totalLen > 0) {
      this.#tryDeliver()
      return
    }
    const err = this.error ?? new RedDBError('CONNECTION_CLOSED', 'redwire: connection closed')
    while (this.waiters.length > 0) {
      this.waiters.shift().reject(err)
    }
  }

  #flatten() {
    if (this.chunks.length === 1) return this.chunks[0]
    const out = new Uint8Array(this.totalLen)
    let off = 0
    for (const c of this.chunks) {
      out.set(c, off)
      off += c.length
    }
    this.chunks = [out]
    return out
  }
}

function writeAll(socket, bytes) {
  return new Promise((resolve, reject) => {
    socket.write(bytes, (err) => (err ? reject(err) : resolve()))
  })
}

// ---------------------------------------------------------------------------
// JSON helpers — handshake payloads use JSON for now (CBOR follow-up)
// ---------------------------------------------------------------------------

function jsonBytes(obj) {
  return new TextEncoder().encode(JSON.stringify(obj))
}

function jsonOf(bytes) {
  if (!bytes || bytes.length === 0) return null
  try {
    return JSON.parse(new TextDecoder().decode(bytes))
  } catch {
    return null
  }
}

function isSelectQuery(sql) {
  return typeof sql === 'string' && /^\s*select\b/i.test(sql)
}

export function decodeResultPayload(payload) {
  const json = jsonOf(payload)
  if (json) return json
  return decodeBinaryResultPayload(payload)
}

function decodeBinaryResultPayload(payload) {
  if (!(payload instanceof Uint8Array)) {
    payload = new Uint8Array(payload)
  }
  const view = new DataView(payload.buffer, payload.byteOffset, payload.byteLength)
  const dec = new TextDecoder()
  let pos = 0

  const read = (n, label) => {
    if (pos + n > payload.length) {
      throw new RedDBError('PROTOCOL', `Result payload truncated while reading ${label}`)
    }
    const start = pos
    pos += n
    return start
  }
  const readU16 = (label) => view.getUint16(read(2, label), true)
  const readU32 = (label) => view.getUint32(read(4, label), true)
  const readI64 = (label) => safeBigIntToJs(view.getBigInt64(read(8, label), true))
  const readU64 = (label) => safeBigIntToJs(view.getBigUint64(read(8, label), true))
  const readF64 = (label) => view.getFloat64(read(8, label), true)
  const readText = (n, label) => dec.decode(payload.subarray(read(n, label), pos))

  const columnCount = readU16('column count')
  const columns = []
  for (let i = 0; i < columnCount; i += 1) {
    const len = readU16(`column ${i} length`)
    columns.push(readText(len, `column ${i} name`))
  }

  const rowCount = readU32('row count')
  const rows = []
  for (let rowIndex = 0; rowIndex < rowCount; rowIndex += 1) {
    const row = {}
    for (const column of columns) {
      row[column] = readBinaryValue()
    }
    rows.push(row)
  }

  return {
    ok: true,
    statement: 'SELECT',
    affected: 0,
    columns,
    rows,
  }

  function readBinaryValue() {
    const tag = payload[read(1, 'value tag')]
    switch (tag) {
      case BinaryTag.Null:
        return null
      case BinaryTag.I64:
        return readI64('i64 value')
      case BinaryTag.U64:
        return readU64('u64 value')
      case BinaryTag.F64:
        return readF64('f64 value')
      case BinaryTag.Text: {
        const len = readU32('text length')
        return readText(len, 'text value')
      }
      case BinaryTag.Bool:
        return payload[read(1, 'bool value')] !== 0
      default:
        throw new RedDBError('PROTOCOL', `Result payload has unknown value tag ${tag}`)
    }
  }
}

function safeBigIntToJs(value) {
  if (
    value >= BigInt(Number.MIN_SAFE_INTEGER)
    && value <= BigInt(Number.MAX_SAFE_INTEGER)
  ) {
    return Number(value)
  }
  return value
}

/**
 * Encode the binary bulk-insert payload body (raw, no RedWire frame
 * header — the body is wrapped by the caller as a `BulkInsertBinary`
 * frame).
 * Layout: `[coll_len u16][coll_bytes][ncols u16]
 *          [(name_len u16)(name_bytes)]*ncols
 *          [nrows u32]
 *          [(tag u8)(value)]*ncols * nrows`
 */
function encodeBinaryBulk(collection, columns, rows) {
  const enc = new TextEncoder()
  const collBytes = enc.encode(collection)
  // Pre-encode column names + their length prefixes.
  const colChunks = columns.map((c) => enc.encode(c))
  let total = 2 + collBytes.length + 2
  for (const cb of colChunks) total += 2 + cb.length
  total += 4
  // Estimate row size — we'll resize if needed.
  for (const row of rows) {
    if (!Array.isArray(row) || row.length !== columns.length) {
      throw new TypeError(
        `bulkInsertBinary: each row must be an array of length ${columns.length}`,
      )
    }
    for (const cell of row) {
      total += sizeOfBinaryCell(cell)
    }
  }
  const buf = new Uint8Array(total)
  const view = new DataView(buf.buffer)
  let pos = 0
  view.setUint16(pos, collBytes.length, true); pos += 2
  buf.set(collBytes, pos); pos += collBytes.length
  view.setUint16(pos, colChunks.length, true); pos += 2
  for (const cb of colChunks) {
    view.setUint16(pos, cb.length, true); pos += 2
    buf.set(cb, pos); pos += cb.length
  }
  view.setUint32(pos, rows.length, true); pos += 4
  for (const row of rows) {
    for (const cell of row) {
      pos = writeBinaryCell(buf, view, pos, cell, enc)
    }
  }
  return buf
}

function sizeOfBinaryCell(cell) {
  if (!Array.isArray(cell) || cell.length !== 2) {
    throw new TypeError('bulkInsertBinary cell must be [tag, value]')
  }
  const [tag] = cell
  switch (tag) {
    case 0: return 1
    case 1: return 1 + 8
    case 2: return 1 + 8
    case 3: {
      const v = cell[1]
      const bytes = typeof v === 'string' ? new TextEncoder().encode(v).length : 0
      return 1 + 4 + bytes
    }
    case 4: return 1 + 1
    case 5: return 1 + 8
    default: throw new RedDBError('UNKNOWN_BINARY_TAG', `tag=${tag}`)
  }
}

function writeBinaryCell(buf, view, pos, cell, enc) {
  const [tag, value] = cell
  buf[pos++] = tag
  switch (tag) {
    case 0: // Null
      return pos
    case 1: { // I64
      const bi = typeof value === 'bigint' ? value : BigInt(value)
      view.setBigInt64(pos, bi, true)
      return pos + 8
    }
    case 2: { // F64
      view.setFloat64(pos, Number(value), true)
      return pos + 8
    }
    case 3: { // Text
      const bytes = enc.encode(String(value))
      view.setUint32(pos, bytes.length, true); pos += 4
      buf.set(bytes, pos)
      return pos + bytes.length
    }
    case 4: { // Bool
      buf[pos] = value ? 1 : 0
      return pos + 1
    }
    case 5: { // U64
      const bi = typeof value === 'bigint' ? value : BigInt(value)
      view.setBigUint64(pos, bi, true)
      return pos + 8
    }
    default:
      throw new RedDBError('UNKNOWN_BINARY_TAG', `tag=${tag}`)
  }
}

// ---------------------------------------------------------------------------
// QueryWithParams payload codec — mirrors `reddb_wire::query_with_params`
// ---------------------------------------------------------------------------

const MAX_VALUE_PAYLOAD_LEN = MAX_FRAME_SIZE
const MAX_PARAM_COUNT = 65_536

/**
 * Encode the `QueryWithParams` payload body.
 * Layout: `[u32 sql_len LE][utf-8 sql][u32 param_count LE][N encoded values]`
 */
export function encodeQueryWithParams(sql, params) {
  if (typeof sql !== 'string') throw new TypeError('encodeQueryWithParams: sql must be a string')
  if (!Array.isArray(params)) throw new TypeError('encodeQueryWithParams: params must be an array')
  if (params.length > MAX_PARAM_COUNT) {
    throw new RedDBError('PARAM_COUNT_OVER_LIMIT', `param_count ${params.length} > ${MAX_PARAM_COUNT}`)
  }
  const sqlBytes = new TextEncoder().encode(sql)
  if (sqlBytes.length > MAX_VALUE_PAYLOAD_LEN) {
    throw new RedDBError('PAYLOAD_TOO_LARGE', `sql_len ${sqlBytes.length} > ${MAX_VALUE_PAYLOAD_LEN}`)
  }
  const valueBlobs = params.map(encodeValue)
  let total = 4 + sqlBytes.length + 4
  for (const vb of valueBlobs) total += vb.length
  const buf = new Uint8Array(total)
  const view = new DataView(buf.buffer)
  let pos = 0
  view.setUint32(pos, sqlBytes.length, true); pos += 4
  buf.set(sqlBytes, pos); pos += sqlBytes.length
  view.setUint32(pos, valueBlobs.length, true); pos += 4
  for (const vb of valueBlobs) { buf.set(vb, pos); pos += vb.length }
  return buf
}

/**
 * Encode a single wire `Value`. Mirrors `reddb_wire::value::encode`.
 *
 * Accepts native JS values + the JSON envelopes produced by
 * `serializeParam` so the SDK can pass through a single shape:
 *   - `null` / `undefined`               → Null
 *   - `boolean`                          → Bool
 *   - `bigint`                           → Int (i64)
 *   - `number` integer (safe range)      → Int; otherwise → Float
 *   - `string`                           → Text
 *   - `Uint8Array` / `Buffer`            → Bytes
 *   - `Float32Array` / `Array<number>`   → Vector (f32)
 *   - `{ $bytes: <base64> }`             → Bytes
 *   - `{ $ts: <unix-seconds> }`          → Timestamp
 *   - `{ $uuid: <hyphenated> }`          → Uuid
 *   - other plain object/array           → Json (canonical bytes)
 */
export function encodeValue(v) {
  if (v === null || v === undefined) return Uint8Array.of(ValueTag.Null)
  if (typeof v === 'boolean') return Uint8Array.of(ValueTag.Bool, v ? 1 : 0)
  if (typeof v === 'bigint') {
    const out = new Uint8Array(1 + 8)
    out[0] = ValueTag.Int
    new DataView(out.buffer).setBigInt64(1, v, true)
    return out
  }
  if (typeof v === 'number') {
    if (Number.isInteger(v) && v >= -(2 ** 53) && v <= 2 ** 53) {
      const out = new Uint8Array(1 + 8)
      out[0] = ValueTag.Int
      new DataView(out.buffer).setBigInt64(1, BigInt(v), true)
      return out
    }
    const out = new Uint8Array(1 + 8)
    out[0] = ValueTag.Float
    new DataView(out.buffer).setFloat64(1, v, true)
    return out
  }
  if (typeof v === 'string') return encodeLenPrefixed(ValueTag.Text, new TextEncoder().encode(v))
  if (v instanceof Uint8Array) return encodeLenPrefixed(ValueTag.Bytes, v)
  if (typeof Buffer !== 'undefined' && v instanceof Buffer) {
    return encodeLenPrefixed(ValueTag.Bytes, new Uint8Array(v.buffer, v.byteOffset, v.byteLength))
  }
  if (v instanceof Float32Array) return encodeVector(v)
  if (v instanceof Float64Array) return encodeVector(Float32Array.from(v))
  if (Array.isArray(v) && v.every((x) => typeof x === 'number')) {
    return encodeVector(Float32Array.from(v))
  }
  if (typeof v === 'object') {
    const keys = Object.keys(v)
    if (keys.length === 1) {
      const k = keys[0]
      if (k === '$bytes' && typeof v.$bytes === 'string') {
        return encodeLenPrefixed(ValueTag.Bytes, base64ToBytes(v.$bytes))
      }
      if (k === '$ts' && (
        (typeof v.$ts === 'number' && Number.isFinite(v.$ts))
        || typeof v.$ts === 'string'
      )) {
        const out = new Uint8Array(1 + 8)
        out[0] = ValueTag.Timestamp
        const raw = typeof v.$ts === 'string' ? BigInt(v.$ts) : BigInt(Math.trunc(v.$ts))
        new DataView(out.buffer).setBigInt64(1, raw, true)
        return out
      }
      if (k === '$uuid' && typeof v.$uuid === 'string') {
        const bytes = parseUuidHyphenated(v.$uuid)
        const out = new Uint8Array(1 + 16)
        out[0] = ValueTag.Uuid
        out.set(bytes, 1)
        return out
      }
    }
    return encodeLenPrefixed(ValueTag.Json, new TextEncoder().encode(canonicalJson(v)))
  }
  throw new RedDBError('UNSUPPORTED_PARAM', `cannot encode param of type ${typeof v}`)
}

function encodeLenPrefixed(tag, bytes) {
  if (bytes.length > MAX_VALUE_PAYLOAD_LEN) {
    throw new RedDBError('PAYLOAD_TOO_LARGE', `value len ${bytes.length} > ${MAX_VALUE_PAYLOAD_LEN}`)
  }
  const out = new Uint8Array(1 + 4 + bytes.length)
  out[0] = tag
  new DataView(out.buffer).setUint32(1, bytes.length, true)
  out.set(bytes, 5)
  return out
}

function encodeVector(f32) {
  if (f32.length * 4 > MAX_VALUE_PAYLOAD_LEN) {
    throw new RedDBError('PAYLOAD_TOO_LARGE', `vector bytes ${f32.length * 4} > ${MAX_VALUE_PAYLOAD_LEN}`)
  }
  const out = new Uint8Array(1 + 4 + f32.length * 4)
  out[0] = ValueTag.Vector
  const view = new DataView(out.buffer)
  view.setUint32(1, f32.length, true)
  for (let i = 0; i < f32.length; i++) {
    view.setFloat32(5 + i * 4, f32[i], true)
  }
  return out
}

function base64ToBytes(s) {
  if (typeof Buffer !== 'undefined') {
    const b = Buffer.from(s, 'base64')
    return new Uint8Array(b.buffer, b.byteOffset, b.byteLength)
  }
  // eslint-disable-next-line no-undef
  const bin = atob(s)
  const out = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i)
  return out
}

function parseUuidHyphenated(s) {
  const hex = s.replace(/-/g, '')
  if (hex.length !== 32 || /[^0-9a-fA-F]/.test(hex)) {
    throw new RedDBError('UUID_INVALID', `bad uuid: ${s}`)
  }
  const out = new Uint8Array(16)
  for (let i = 0; i < 16; i++) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16)
  return out
}

/** Stable JSON serialization with sorted keys — matches the server's
 * canonical `crate::json` output so round-tripped Json values compare
 * byte-equal. */
function canonicalJson(v) {
  if (v === null) return 'null'
  if (typeof v === 'number') return Number.isFinite(v) ? String(v) : 'null'
  if (typeof v === 'string') return JSON.stringify(v)
  if (typeof v === 'boolean') return v ? 'true' : 'false'
  if (Array.isArray(v)) return '[' + v.map(canonicalJson).join(',') + ']'
  if (typeof v === 'object') {
    const keys = Object.keys(v).sort()
    return '{' + keys.map((k) => JSON.stringify(k) + ':' + canonicalJson(v[k])).join(',') + '}'
  }
  return 'null'
}

function jsonReason(bytes) {
  const v = jsonOf(bytes)
  if (v && typeof v === 'object' && typeof v.reason === 'string') {
    return v.reason
  }
  return null
}
