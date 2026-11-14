/**
 * RedDB Wire Protocol Client for Bun.
 *
 * Uses Bun's native TCP socket and speaks the RedWire v1 frame protocol.
 */

import { RoutingClient } from '@reddb-io/client/routing'
import { decodeTopology } from '@reddb-io/client/topology'

export type ReadOptions = { consistency: 'local' | 'strong' | 'bounded_staleness'; maxLagMs?: number }

const MAGIC = 0xfe
const SUPPORTED_VERSION = 0x01
const FRAME_HEADER_SIZE = 16
const MAX_FRAME_SIZE = 16 * 1024 * 1024
const FEATURE_PARAMS = 0x0000_0001
const FEATURE_LANDMARK_PARAMS = 0x0000_0010

const MSG_QUERY = 0x01
const MSG_RESULT = 0x02
const MSG_ERROR = 0x03
const MSG_BULK_INSERT = 0x04
const MSG_BULK_OK = 0x05
const MSG_HELLO = 0x10
const MSG_HELLO_ACK = 0x11
const MSG_AUTH_RESPONSE = 0x13
const MSG_AUTH_OK = 0x14
const MSG_AUTH_FAIL = 0x15
const MSG_BYE = 0x16
const MSG_QUERY_WITH_PARAMS = 0x28

const VALUE_NULL = 0x00
const VALUE_BOOL = 0x01
const VALUE_INT = 0x02
const VALUE_FLOAT = 0x03
const VALUE_TEXT = 0x04
const VALUE_BYTES = 0x05
const VALUE_VECTOR = 0x06
const VALUE_JSON = 0x07
const VALUE_TIMESTAMP = 0x08
const VALUE_UUID = 0x09
const VALUE_LANDMARK_SET = 0x0f

type QueryParam =
  | null
  | boolean
  | number
  | string
  | Uint8Array
  | Date
  | Float32Array
  | Float64Array
  | number[]
  | Record<string, unknown>

interface PendingRequest {
  resolve: (value: { type: number; payload: Buffer }) => void
  reject: (error: Error) => void
}

export class RedDBError extends Error {
  code: string

  constructor(code: string, message: string) {
    super(message)
    this.name = 'RedDBError'
    this.code = code
  }
}

export class RedDBConnection {
  private socket: ReturnType<typeof Bun.connect> extends Promise<infer T> ? T : never
  private pending: PendingRequest[] = []
  private chunks: (Buffer | null)[] = []
  private chunkIndex = 0
  private chunkOffset = 0
  private bufferedBytes = 0
  private frameLength = 0
  private frameType = 0
  private failure: Error | null = null
  private nextCorr = 1n
  private serverFeatures = 0
  private routing?: RoutingClient

  constructor(socket: any) {
    this.socket = socket
  }

  _onData(chunk: Buffer) {
    if (this.failure || chunk.length === 0) return
    this.chunks.push(chunk)
    this.bufferedBytes += chunk.length
    this._tryResolve()
  }

  _onClose() {
    this._fail(new RedDBError('CONNECTION_CLOSED', 'redwire: connection closed'))
  }

  _onError(error: Error) {
    this._fail(error)
  }

  private _tryResolve() {
    while (this.bufferedBytes >= FRAME_HEADER_SIZE && this.pending.length > 0) {
      let payload: Buffer
      let msgType: number
      try {
        if (this.frameLength === 0) this._readHeader()
        if (this.bufferedBytes < this.frameLength) break
        msgType = this.frameType
        this._consume(FRAME_HEADER_SIZE)
        payload = Buffer.allocUnsafe(this.frameLength - FRAME_HEADER_SIZE)
        this._consume(payload.length, payload)
        this.frameLength = 0
      } catch (error) {
        this._fail(error as Error)
        return
      }
      const { resolve, reject } = this.pending.shift()!
      if (msgType === MSG_ERROR || msgType === MSG_AUTH_FAIL) {
        reject(new RedDBError('ENGINE', payload.toString('utf8')))
      } else {
        resolve({ type: msgType, payload })
      }
    }
  }

  private _readHeader() {
    const first = this.chunks[this.chunkIndex]!
    let header = first
    let offset = this.chunkOffset
    if (first.length - offset < 5) {
      // Only length and kind are needed; the rest of the 16-byte header is discarded later.
      header = Buffer.allocUnsafe(5)
      let written = 0
      for (let index = this.chunkIndex; written < header.length; index++) {
        const chunk = this.chunks[index]!
        const start = index === this.chunkIndex ? this.chunkOffset : 0
        const count = Math.min(header.length - written, chunk.length - start)
        written += chunk.copy(header, written, start, start + count)
      }
      offset = 0
    }
    const length = header.readUInt32LE(offset)
    if (length < FRAME_HEADER_SIZE || length > MAX_FRAME_SIZE) {
      throw new RedDBError('FRAME_INVALID_LENGTH', `length=${length}`)
    }
    this.frameLength = length
    this.frameType = header[offset + 4]
  }

  private _consume(length: number, output?: Buffer) {
    if (length === 0) return
    let written = 0
    while (written < length) {
      const chunk = this.chunks[this.chunkIndex]!
      const count = Math.min(length - written, chunk.length - this.chunkOffset)
      if (output) chunk.copy(output, written, this.chunkOffset, this.chunkOffset + count)
      written += count
      this.chunkOffset += count
      if (this.chunkOffset === chunk.length) {
        this.chunks[this.chunkIndex++] = null
        this.chunkOffset = 0
      }
    }
    this.bufferedBytes -= length
    if (this.chunkIndex * 2 >= this.chunks.length) {
      this.chunks = this.chunks.slice(this.chunkIndex)
      this.chunkIndex = 0
    }
  }

  private _fail(error: Error) {
    if (this.failure) return
    this.failure = error
    this.chunks = []
    this.chunkIndex = 0
    this.chunkOffset = 0
    this.bufferedBytes = 0
    this.frameLength = 0
    while (this.pending.length > 0) this.pending.shift()!.reject(error)
    try { this.socket.end() } catch { /* The peer may already have closed the socket. */ }
  }

  private _send(msgType: number, payload: Buffer): Promise<{ type: number; payload: Buffer }> {
    if (this.failure) return Promise.reject(this.failure)
    return new Promise((resolve, reject) => {
      const corr = this.nextCorr
      this.nextCorr += 1n
      const request = encodeFrame(msgType, corr, payload)
      this.pending.push({ resolve, reject })
      try { this.socket.write(request) }
      catch (error) { this._fail(error as Error) }
    })
  }

  async _handshake(token?: string) {
    this.socket.write(Buffer.from([MAGIC, SUPPORTED_VERSION]))
    const hello = jsonBytes({
      versions: [SUPPORTED_VERSION],
      auth_methods: token ? ['bearer'] : ['anonymous'],
      features: 0,
      client_name: 'reddb-bun/1.0',
    })
    const ack = await this._send(MSG_HELLO, hello)
    if (ack.type !== MSG_HELLO_ACK) {
      throw new RedDBError('PROTOCOL', `expected HelloAck, got ${ack.type}`)
    }
    const parsedAck = jsonOf(ack.payload) ?? {}
    const chosenAuth = parsedAck.auth
    if (chosenAuth !== (token ? 'bearer' : 'anonymous')) {
      throw new RedDBError('AUTH_REFUSED', `unsupported auth method: ${chosenAuth}`)
    }

    const authOk = await this._send(MSG_AUTH_RESPONSE, token ? jsonBytes({ token }) : Buffer.alloc(0))
    if (authOk.type !== MSG_AUTH_OK) {
      throw new RedDBError('PROTOCOL', `expected AuthOk, got ${authOk.type}`)
    }
    const session = jsonOf(authOk.payload) ?? {}
    this.serverFeatures = numberOr(session.features, numberOr(parsedAck.features, 0))
  }

  supportsParams(): boolean {
    return (this.serverFeatures & FEATURE_PARAMS) === FEATURE_PARAMS
  }

  async queryWithOptions(sql: string, options: ReadOptions, ...params: QueryParam[]): Promise<string> {
    if (!this.routing) throw new RedDBError('CONSISTENCY_UNSUPPORTED', 'use connect() for routed reads')
    return this.routing.queryWithOptions(sql, options, normalizeQueryParams(params))
  }

  static async routed(address: string, initial: RedDBConnection, tls?: { ca?: string; rejectUnauthorized: boolean }, token?: string) {
    const protocol = tls ? 'reds' : 'red'
    const seed = new URL(`${protocol}://${address}`).href.replace(/\/$/, '')
    const adapt = (raw: RedDBConnection) => ({
      binaryQueryParams: true,
      async call(method: string, params: any = {}) {
        if (method === 'topology') return raw.topologyFor(protocol)
        if (method === 'health') { await raw.query('SELECT 1'); return {} }
        if (method === 'query') return raw.query(params.sql, params.params ?? [])
        if (method === 'bulkInsert') return raw.bulkInsert(params.collection, params.payloads)
        throw new RedDBError('UNSUPPORTED_OPERATION', method)
      },
      async close() { raw.close() },
    })
    const router = new RoutingClient(seed, adapt(initial), async (endpoint: string, signal: AbortSignal) => {
      const uri = new URL(endpoint)
      const pending = connectDirect(`${uri.hostname}:${uri.port}`, tls, token, signal)
      const raw = await pending
      if (signal.aborted) { raw.close(); throw new RedDBError('CLOSED', 'connection canceled') }
      return adapt(raw)
    }, protocol)
    try {
      await router.initialize()
      const facade = new RedDBConnection(null)
      facade.routing = router
      facade.serverFeatures = initial.serverFeatures
      return facade
    } catch (error) { await router.close(); throw error }
  }

  private async topologyFor(protocol: string) {
    if (!(this.serverFeatures & 2)) return null
    const response = await this._send(0x32, Buffer.from(protocol))
    if (response.type !== MSG_RESULT) throw new RedDBError('PROTOCOL', 'expected topology Result')
    return decodeTopology(response.payload)
  }

  async query(sql: string, params: QueryParam[]): Promise<string>
  async query(sql: string, ...params: QueryParam[]): Promise<string>
  async query(sql: string, ...params: unknown[]): Promise<string> {
    const wireParams = normalizeQueryParams(params)
    if (this.routing) return this.routing.call('query', { sql, params: wireParams })
    const hasParams = wireParams.length > 0
    const fullResult = this.supportsParams()
    if (hasParams && !fullResult) {
      throw new RedDBError(
        'PARAMS_UNSUPPORTED',
        'server did not advertise FEATURE_PARAMS; upgrade the server',
      )
    }
    if (wireParams.some(isLandmarkParam) &&
        (this.serverFeatures & FEATURE_LANDMARK_PARAMS) !== FEATURE_LANDMARK_PARAMS) {
      throw new RedDBError(
        'PARAMS_UNSUPPORTED',
        'server did not advertise FEATURE_LANDMARK_PARAMS',
      )
    }
    // Legacy Query returns a summary only. Capable servers return rows via
    // QueryWithParams even when the parameter vector is empty.
    const resp = await this._send(
      fullResult ? MSG_QUERY_WITH_PARAMS : MSG_QUERY,
      fullResult
        ? encodeQueryWithParams(sql, wireParams)
        : Buffer.from(sql, 'utf8'),
    )
    if (resp.type !== MSG_RESULT) {
      throw new RedDBError('PROTOCOL', `expected Result, got ${resp.type}`)
    }
    return resp.payload.toString('utf8')
  }

  async execute(sql: string, params: QueryParam[]): Promise<string>
  async execute(sql: string, ...params: QueryParam[]): Promise<string>
  async execute(sql: string, ...params: unknown[]): Promise<string> {
    return this.query(sql, ...(params as QueryParam[]))
  }

  async queryParsed(sql: string, params: QueryParam[]): Promise<any>
  async queryParsed(sql: string, ...params: QueryParam[]): Promise<any>
  async queryParsed(sql: string, ...params: unknown[]): Promise<any> {
    return JSON.parse(await this.query(sql, ...(params as QueryParam[])))
  }

  async queryRaw(sql: string, params: QueryParam[]): Promise<number>
  async queryRaw(sql: string, ...params: QueryParam[]): Promise<number>
  async queryRaw(sql: string, ...params: unknown[]): Promise<number> {
    const resp = await this.query(sql, ...(params as QueryParam[]))
    return Buffer.byteLength(resp)
  }

  async bulkInsert(collection: string, jsonPayloads: string[]): Promise<number> {
    if (this.routing) return this.routing.call('bulkInsert', { collection, payloads: jsonPayloads })
    const payloads = jsonPayloads.map((p) => JSON.parse(p))
    const resp = await this._send(
      MSG_BULK_INSERT,
      jsonBytes({ collection, payloads }),
    )
    if (resp.type !== MSG_BULK_OK) {
      throw new RedDBError('PROTOCOL', `expected BulkOk, got ${resp.type}`)
    }
    const json = jsonOf(resp.payload)
    if (json && typeof json.affected === 'number') return json.affected
    if (resp.payload.length >= 8) return Number(resp.payload.readBigUInt64LE(0))
    return 0
  }

  close() {
    if (this.routing) { void this.routing.close(); return }
    if (this.failure) return
    try {
      this.socket.write(encodeFrame(MSG_BYE, this.nextCorr, Buffer.alloc(0)))
      this.nextCorr += 1n
    } finally {
      this._onClose()
    }
  }
}

export async function connect(addr: string, opts: { token?: string } = {}): Promise<RedDBConnection> {
  return RedDBConnection.routed(addr, await connectDirect(addr, undefined, opts.token), undefined, opts.token)
}

/**
 * Connect via TLS-encrypted wire protocol.
 * @param addr - Host:port (e.g. "127.0.0.1:55555")
 * @param opts - TLS options
 */
export async function connectTls(
  addr: string,
  opts: { ca?: string; rejectUnauthorized?: boolean; token?: string } = {},
): Promise<RedDBConnection> {
  const tls = {
    ca: opts.ca,
    rejectUnauthorized: opts.rejectUnauthorized ?? true,
  }
  return RedDBConnection.routed(addr, await connectDirect(addr, tls, opts.token), tls, opts.token)
}

async function connectDirect(addr: string, tls?: { ca?: string; rejectUnauthorized: boolean }, token?: string, signal?: AbortSignal) {
  if (signal?.aborted) throw new RedDBError('CLOSED', 'connection canceled')
  const uri = new URL(`red://${addr}`)
  const host = uri.hostname.replace(/^\[|\]$/g, '')
  const port = Number(uri.port)
  let connRef: RedDBConnection | null = null
  let earlyFailure: Error | null = null
  const fail = (error: Error) => {
    if (connRef) connRef._onError(error)
    else earlyFailure ??= error
  }
  const closed = () => fail(new RedDBError('CONNECTION_CLOSED', 'redwire: connection closed'))

  const socket = await Bun.connect({
    hostname: host,
    port,
    ...(tls ? { tls } : {}),
    socket: {
      data(_socket, data) {
        connRef?._onData(Buffer.from(data))
      },
      error(_socket, error) {
        fail(error)
      },
      close: closed,
      end: closed,
      open() {},
    },
  })

  connRef = new RedDBConnection(socket)
  if (earlyFailure) connRef._onError(earlyFailure)
  const abort = () => connRef?._onError(new RedDBError('CLOSED', 'connection canceled'))
  signal?.addEventListener('abort', abort, { once: true })
  if (signal?.aborted) abort()
  try { await connRef._handshake(token) }
  catch (error) { connRef._onError(error as Error); throw error }
  finally { signal?.removeEventListener('abort', abort) }
  return connRef
}

function encodeFrame(kind: number, correlationId: bigint, payload: Buffer): Buffer {
  const totalLen = FRAME_HEADER_SIZE + payload.length
  if (totalLen > MAX_FRAME_SIZE) {
    throw new RedDBError('FRAME_TOO_LARGE', `frame ${totalLen} > ${MAX_FRAME_SIZE}`)
  }
  const out = Buffer.alloc(totalLen)
  out.writeUInt32LE(totalLen, 0)
  out[4] = kind
  out[5] = 0
  out.writeUInt16LE(0, 6)
  out.writeBigUInt64LE(correlationId, 8)
  payload.copy(out, FRAME_HEADER_SIZE)
  return out
}

function normalizeQueryParams(args: unknown[]): QueryParam[] {
  if (args.length === 0) return []
  if (args.length === 1 && Array.isArray(args[0])) return args[0] as QueryParam[]
  return args as QueryParam[]
}

function encodeQueryWithParams(sql: string, params: QueryParam[]): Buffer {
  if (params.length > 65_536) {
    throw new RedDBError('PARAM_COUNT_OVER_LIMIT', `param_count ${params.length} > 65536`)
  }
  const sqlBytes = Buffer.from(sql, 'utf8')
  const values = params.map(encodeValue)
  let total = 4 + sqlBytes.length + 4
  for (const value of values) total += value.length
  const out = Buffer.alloc(total)
  let pos = 0
  out.writeUInt32LE(sqlBytes.length, pos); pos += 4
  sqlBytes.copy(out, pos); pos += sqlBytes.length
  out.writeUInt32LE(values.length, pos); pos += 4
  for (const value of values) {
    value.copy(out, pos)
    pos += value.length
  }
  return out
}

function encodeValue(value: QueryParam): Buffer {
  if (value === null) return Buffer.from([VALUE_NULL])
  if (typeof value === 'boolean') return Buffer.from([VALUE_BOOL, value ? 1 : 0])
  if (typeof value === 'number') {
    const out = Buffer.alloc(9)
    if (Number.isInteger(value) && value >= -(2 ** 53) && value <= 2 ** 53) {
      out[0] = VALUE_INT
      out.writeBigInt64LE(BigInt(value), 1)
    } else {
      out[0] = VALUE_FLOAT
      out.writeDoubleLE(value, 1)
    }
    return out
  }
  if (typeof value === 'string') return encodeBytes(VALUE_TEXT, Buffer.from(value, 'utf8'))
  if (value instanceof Uint8Array) {
    return encodeBytes(VALUE_BYTES, Buffer.from(value.buffer, value.byteOffset, value.byteLength))
  }
  if (value instanceof Date) {
    if (Number.isNaN(value.getTime())) {
      throw new RedDBError('UNSUPPORTED_PARAM', 'cannot encode invalid Date query parameter')
    }
    const out = Buffer.alloc(9)
    out[0] = VALUE_TIMESTAMP
    out.writeBigInt64LE(BigInt(value.getTime()) * 1_000_000n, 1)
    return out
  }
  if (value instanceof Float32Array) return encodeVector(value)
  if (value instanceof Float64Array) return encodeVector(Float32Array.from(value))
  if (Array.isArray(value)) {
    if (value.every((item) => typeof item === 'number')) {
      return encodeVector(Float32Array.from(value))
    }
    throw new RedDBError('UNSUPPORTED_PARAM', 'array query parameters must contain only numbers')
  }
  if (typeof value === 'object' && Object.getPrototypeOf(value) === Object.prototype) {
    const record = value as Record<string, unknown>
    const keys = Object.keys(record)
    if (keys.length === 1 && typeof record.$uuid === 'string') {
      const out = Buffer.alloc(17)
      out[0] = VALUE_UUID
      Buffer.from(record.$uuid.replace(/-/g, ''), 'hex').copy(out, 1)
      return out
    }
    if (keys.length === 1 && record.$landmark_set !== undefined) {
      return encodeLandmarkSet(record.$landmark_set)
    }
    return encodeBytes(VALUE_JSON, Buffer.from(canonicalJson(value), 'utf8'))
  }
  throw new RedDBError('UNSUPPORTED_PARAM', `cannot encode query parameter of type ${typeof value}`)
}

function isLandmarkParam(value: QueryParam): boolean {
  return typeof value === 'object' && value !== null &&
    Object.getPrototypeOf(value) === Object.prototype &&
    Object.keys(value).length === 1 && '$landmark_set' in value
}

function encodeLandmarkSet(raw: unknown): Buffer {
  if (typeof raw !== 'object' || raw === null || Array.isArray(raw)) {
    throw new RedDBError('UNSUPPORTED_PARAM', 'invalid landmark geometry')
  }
  const value = raw as Record<string, unknown>
  if (Object.keys(value).length !== 4 ||
      (value.dimensions !== 2 && value.dimensions !== 3) ||
      !Array.isArray(value.pairs) || value.pairs.length < 2 ||
      !Array.isArray(value.midline) ||
      value.pairs.length * 2 + value.midline.length > 4096 ||
      typeof value.space_id !== 'string') {
    throw new RedDBError('UNSUPPORTED_PARAM', 'invalid landmark geometry')
  }
  const compactSpace = value.space_id.replace(/-/g, '')
  if (!/^[0-9a-fA-F]{32}$/.test(compactSpace)) {
    throw new RedDBError('UNSUPPORTED_PARAM', 'invalid landmark geometry')
  }
  const dimensions = value.dimensions
  const points: number[][] = []
  for (const rawPair of value.pairs) {
    if (typeof rawPair !== 'object' || rawPair === null || Array.isArray(rawPair)) {
      throw new RedDBError('UNSUPPORTED_PARAM', 'invalid landmark geometry')
    }
    const pair = rawPair as Record<string, unknown>
    if (Object.keys(pair).length !== 2 || !('left' in pair) || !('right' in pair)) {
      throw new RedDBError('UNSUPPORTED_PARAM', 'invalid landmark geometry')
    }
    points.push(validateLandmarkPoint(pair.left, dimensions))
    points.push(validateLandmarkPoint(pair.right, dimensions))
  }
  for (const point of value.midline) points.push(validateLandmarkPoint(point, dimensions))
  const out = Buffer.alloc(1 + 16 + 1 + 4 + 4 + points.length * dimensions * 4)
  out[0] = VALUE_LANDMARK_SET
  Buffer.from(compactSpace, 'hex').copy(out, 1)
  out[17] = dimensions
  out.writeUInt32LE(value.pairs.length, 18)
  out.writeUInt32LE(value.midline.length, 22)
  let offset = 26
  for (const point of points) {
    for (const coordinate of point) {
      out.writeFloatLE(coordinate, offset)
      offset += 4
    }
  }
  return out
}

function validateLandmarkPoint(raw: unknown, dimensions: 2 | 3): number[] {
  if (!Array.isArray(raw) || raw.length !== dimensions ||
      raw.some((coordinate) => typeof coordinate !== 'number' || !Number.isFinite(coordinate))) {
    throw new RedDBError('UNSUPPORTED_PARAM', 'invalid landmark geometry')
  }
  return raw as number[]
}

function encodeBytes(tag: number, bytes: Buffer): Buffer {
  const out = Buffer.alloc(1 + 4 + bytes.length)
  out[0] = tag
  out.writeUInt32LE(bytes.length, 1)
  bytes.copy(out, 5)
  return out
}

function encodeVector(values: Float32Array): Buffer {
  const out = Buffer.alloc(1 + 4 + values.length * 4)
  out[0] = VALUE_VECTOR
  out.writeUInt32LE(values.length, 1)
  for (let i = 0; i < values.length; i++) {
    out.writeFloatLE(values[i], 5 + i * 4)
  }
  return out
}

function canonicalJson(value: unknown): string {
  if (value === null) return 'null'
  if (typeof value === 'number') return Number.isFinite(value) ? String(value) : 'null'
  if (typeof value === 'string') return JSON.stringify(value)
  if (typeof value === 'boolean') return value ? 'true' : 'false'
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`
  if (typeof value === 'object') {
    const record = value as Record<string, unknown>
    return `{${Object.keys(record).sort()
      .map((key) => `${JSON.stringify(key)}:${canonicalJson(record[key])}`)
      .join(',')}}`
  }
  return 'null'
}

function jsonBytes(value: unknown): Buffer {
  return Buffer.from(JSON.stringify(value), 'utf8')
}

function jsonOf(bytes: Buffer): any {
  if (bytes.length === 0) return null
  try {
    return JSON.parse(bytes.toString('utf8'))
  } catch {
    return null
  }
}

function numberOr(value: unknown, fallback: number): number {
  return typeof value === 'number' && Number.isFinite(value) ? value : fallback
}
