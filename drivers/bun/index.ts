/**
 * RedDB Wire Protocol Client for Bun.
 *
 * Uses Bun's native TCP socket and speaks the RedWire v1 frame protocol.
 */

const MAGIC = 0xfe
const SUPPORTED_VERSION = 0x01
const FRAME_HEADER_SIZE = 16
const MAX_FRAME_SIZE = 16 * 1024 * 1024
const FEATURE_PARAMS = 0x0000_0001

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
  private buffer = Buffer.alloc(0)
  private nextCorr = 1n
  private serverFeatures = 0

  constructor(socket: any) {
    this.socket = socket
  }

  _onData(chunk: Buffer) {
    this.buffer = Buffer.concat([this.buffer, chunk])
    this._tryResolve()
  }

  private _tryResolve() {
    while (this.buffer.length >= FRAME_HEADER_SIZE && this.pending.length > 0) {
      const totalLen = this.buffer.readUInt32LE(0)
      if (totalLen < FRAME_HEADER_SIZE || totalLen > MAX_FRAME_SIZE) {
        const { reject } = this.pending.shift()!
        reject(new RedDBError('FRAME_INVALID_LENGTH', `length=${totalLen}`))
        return
      }
      if (this.buffer.length < totalLen) break

      const msgType = this.buffer[4]
      const payload = this.buffer.subarray(FRAME_HEADER_SIZE, totalLen)
      this.buffer = this.buffer.subarray(totalLen)

      const { resolve, reject } = this.pending.shift()!
      if (msgType === MSG_ERROR || msgType === MSG_AUTH_FAIL) {
        reject(new RedDBError('ENGINE', payload.toString('utf8')))
      } else {
        resolve({ type: msgType, payload: Buffer.from(payload) })
      }
    }
  }

  private _send(msgType: number, payload: Buffer): Promise<{ type: number; payload: Buffer }> {
    return new Promise((resolve, reject) => {
      this.pending.push({ resolve, reject })
      const corr = this.nextCorr
      this.nextCorr += 1n
      this.socket.write(encodeFrame(msgType, corr, payload))
    })
  }

  async _handshake() {
    this.socket.write(Buffer.from([MAGIC, SUPPORTED_VERSION]))
    const hello = jsonBytes({
      versions: [SUPPORTED_VERSION],
      auth_methods: ['anonymous', 'bearer'],
      features: 0,
      client_name: 'reddb-bun/1.0',
    })
    const ack = await this._send(MSG_HELLO, hello)
    if (ack.type !== MSG_HELLO_ACK) {
      throw new RedDBError('PROTOCOL', `expected HelloAck, got ${ack.type}`)
    }
    const parsedAck = jsonOf(ack.payload) ?? {}
    const chosenAuth = parsedAck.auth
    if (chosenAuth !== 'anonymous') {
      throw new RedDBError('AUTH_REFUSED', `unsupported auth method: ${chosenAuth}`)
    }

    const authOk = await this._send(MSG_AUTH_RESPONSE, Buffer.alloc(0))
    if (authOk.type !== MSG_AUTH_OK) {
      throw new RedDBError('PROTOCOL', `expected AuthOk, got ${authOk.type}`)
    }
    const session = jsonOf(authOk.payload) ?? {}
    this.serverFeatures = numberOr(session.features, numberOr(parsedAck.features, 0))
  }

  supportsParams(): boolean {
    return (this.serverFeatures & FEATURE_PARAMS) === FEATURE_PARAMS
  }

  async query(sql: string, params: QueryParam[]): Promise<string>
  async query(sql: string, ...params: QueryParam[]): Promise<string>
  async query(sql: string, ...params: unknown[]): Promise<string> {
    const wireParams = normalizeQueryParams(params)
    const hasParams = wireParams.length > 0
    if (hasParams && !this.supportsParams()) {
      throw new RedDBError(
        'PARAMS_UNSUPPORTED',
        'server did not advertise FEATURE_PARAMS; upgrade the server',
      )
    }
    const resp = await this._send(
      hasParams ? MSG_QUERY_WITH_PARAMS : MSG_QUERY,
      hasParams
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
    try {
      this.socket.write(encodeFrame(MSG_BYE, this.nextCorr, Buffer.alloc(0)))
      this.nextCorr += 1n
    } finally {
      this.socket.end()
    }
  }
}

export async function connect(addr: string): Promise<RedDBConnection> {
  return connectWithOptions(addr)
}

/**
 * Connect via TLS-encrypted wire protocol.
 * @param addr - Host:port (e.g. "127.0.0.1:55555")
 * @param opts - TLS options
 */
export async function connectTls(
  addr: string,
  opts: { ca?: string; rejectUnauthorized?: boolean } = {},
): Promise<RedDBConnection> {
  return connectWithOptions(addr, {
    ca: opts.ca,
    rejectUnauthorized: opts.rejectUnauthorized ?? true,
  })
}

async function connectWithOptions(addr: string, tls?: { ca?: string; rejectUnauthorized: boolean }) {
  const [host, portStr] = addr.split(':')
  const port = parseInt(portStr, 10)
  let connRef: RedDBConnection | null = null

  const socket = await Bun.connect({
    hostname: host,
    port,
    ...(tls ? { tls } : {}),
    socket: {
      data(_socket, data) {
        connRef?._onData(Buffer.from(data))
      },
      error(_socket, error) {
        console.error('RedDB wire error:', error)
      },
      close() {},
      open() {},
    },
  })

  connRef = new RedDBConnection(socket)
  await connRef._handshake()
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
    return encodeBytes(VALUE_JSON, Buffer.from(canonicalJson(value), 'utf8'))
  }
  throw new RedDBError('UNSUPPORTED_PARAM', `cannot encode query parameter of type ${typeof value}`)
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
