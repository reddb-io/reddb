/**
 * Minimal gRPC transport for the JS driver.
 *
 * Uses Node's built-in HTTP/2 client and a small protobuf codec for
 * the RedDb RPCs the public `RedDB` surface calls. Keeping this local
 * avoids routing grpc:// through the RedWire frame parser.
 */

import { connect as connectHttp2 } from 'node:http2'
import { Buffer } from 'node:buffer'

import { RedDBError } from './protocol.js'
import { normalizeExactNumbers, serializeJsonValue } from './core/serialization.js'

const SERVICE = '/reddb.v1.RedDb'

export class GrpcRpcClient {
  constructor({ baseUrl, token }) {
    if (typeof baseUrl !== 'string' || baseUrl.length === 0) {
      throw new TypeError('GrpcRpcClient: baseUrl required')
    }
    this.baseUrl = baseUrl.replace(/\/$/, '')
    this.token = token ?? null
    this.session = connectHttp2(this.baseUrl)
    this.session.on('error', () => {})
  }

  setToken(token) {
    this.token = token
  }

  async close() {
    this.session.close()
  }

  async call(method, params = {}) {
    switch (method) {
      case 'query':
        return normalizeQueryReply(await this.#rpc(
          'Query',
          encodeQueryRequest(params.sql ?? '', params.params),
          decodeQueryReply,
        ))
      case 'insert':
        return normalizeEntityReply(await this.#rpc(
          'CreateRow',
          encodeJsonCreateRequest(params.collection, params.payload),
          decodeEntityReply,
        ))
      case 'bulk_insert':
        return normalizeBulkEntityReply(await this.#rpc(
          'BulkCreateRows',
          encodeJsonBulkCreateRequest(params.collection, params.payloads),
          decodeBulkEntityReply,
        ))
      case 'delete':
        return normalizeOperationReply(await this.#rpc(
          'DeleteEntity',
          encodeDeleteEntityRequest(params.collection, params.id),
          decodeOperationReply,
        ))
      case 'health':
      case 'version':
        return normalizeHealthReply(await this.#rpc('Ready', new Uint8Array(), decodeHealthReply))
      default:
        throw new RedDBError(
          'UNKNOWN_METHOD',
          `gRPC transport has no route for method '${method}'`,
        )
    }
  }

  #rpc(name, payload, decode) {
    return new Promise((resolve, reject) => {
      const headers = {
        ':method': 'POST',
        ':path': `${SERVICE}/${name}`,
        'content-type': 'application/grpc',
        te: 'trailers',
      }
      if (this.token) headers.authorization = `Bearer ${this.token}`

      const req = this.session.request(headers)
      const chunks = []
      let responseHeaders = null
      let trailers = null

      req.on('response', (headers) => {
        responseHeaders = headers
      })
      req.on('trailers', (headers) => {
        trailers = headers
      })
      req.on('data', (chunk) => {
        chunks.push(chunk)
      })
      req.on('error', reject)
      req.on('end', () => {
        try {
          const status = Number(responseHeaders?.[':status'] ?? 0)
          const grpcStatus = String(
            trailers?.['grpc-status'] ?? responseHeaders?.['grpc-status'] ?? '0',
          )
          if (status !== 200 || grpcStatus !== '0') {
            const message = String(
              trailers?.['grpc-message']
                ?? responseHeaders?.['grpc-message']
                ?? `gRPC ${name} failed`,
            )
            throw new RedDBError(
              grpcStatus === '0' ? `HTTP_${status}` : `GRPC_${grpcStatus}`,
              decodeURIComponent(message),
            )
          }
          const body = Buffer.concat(chunks)
          resolve(decode(readGrpcMessage(body)))
        } catch (err) {
          reject(err)
        }
      })
      req.end(writeGrpcMessage(payload))
    })
  }
}

function writeGrpcMessage(payload) {
  const out = new Uint8Array(5 + payload.length)
  const view = new DataView(out.buffer)
  out[0] = 0
  view.setUint32(1, payload.length, false)
  out.set(payload, 5)
  return out
}

function readGrpcMessage(body) {
  if (body.length < 5) {
    throw new RedDBError('GRPC_PROTOCOL', 'gRPC response missing message header')
  }
  if (body[0] !== 0) {
    throw new RedDBError('GRPC_PROTOCOL', 'compressed gRPC responses are not supported')
  }
  const len = body.readUInt32BE(1)
  if (body.length < 5 + len) {
    throw new RedDBError('GRPC_PROTOCOL', 'gRPC response truncated')
  }
  return body.subarray(5, 5 + len)
}

function normalizeQueryReply(reply) {
  let parsed = {}
  if (reply.result_json) {
    try {
      parsed = JSON.parse(reply.result_json)
    } catch (err) {
      throw new RedDBError('QUERY_ERROR', `bad gRPC query JSON: ${err.message}`)
    }
  }
  const rows = normalizeExactNumbers(parsed.rows ?? parsed.records ?? [])
  return {
    ok: reply.ok,
    statement: parsed.statement ?? reply.statement ?? '',
    affected: parsed.affected ?? parsed.affected_rows ?? 0,
    columns: parsed.columns ?? reply.columns ?? [],
    rows,
  }
}

function normalizeEntityReply(reply) {
  const rid = safeBigIntToJs(reply.id)
  return {
    ok: reply.ok,
    affected: reply.ok ? 1 : 0,
    rid,
    id: rid,
    entity: normalizeExactNumbers(parseJsonOrNull(reply.entity_json)),
  }
}

function normalizeBulkEntityReply(reply) {
  const rids = reply.items.map((item) => safeBigIntToJs(item.id))
  return {
    ok: reply.ok,
    affected: safeBigIntToJs(reply.count),
    rids,
    ids: rids,
  }
}

function normalizeOperationReply(reply) {
  return { ok: reply.ok, affected: reply.ok ? 1 : 0, message: reply.message }
}

function normalizeHealthReply(reply) {
  return {
    ok: reply.healthy,
    state: reply.state,
    checked_at_unix_ms: safeBigIntToJs(reply.checked_at_unix_ms),
  }
}

function parseJsonOrNull(text) {
  if (!text) return null
  try {
    return JSON.parse(text)
  } catch {
    return null
  }
}

function encodeQueryRequest(sql, params) {
  const fields = [stringField(1, sql)]
  if (Array.isArray(params)) {
    for (const param of params) fields.push(bytesField(4, encodeQueryValue(param)))
  }
  return concat(fields)
}

function encodeQueryValue(value) {
  if (value == null) return bytesField(1, new Uint8Array())
  if (typeof value === 'boolean') return boolField(2, value)
  if (typeof value === 'bigint') return int64Field(3, value)
  if (typeof value === 'number') {
    return Number.isInteger(value) && Number.isSafeInteger(value)
      ? int64Field(3, BigInt(value))
      : doubleField(4, value)
  }
  if (typeof value === 'string') return stringField(5, value)
  if (value instanceof Uint8Array || (typeof Buffer !== 'undefined' && value instanceof Buffer)) {
    return bytesField(6, value)
  }
  if (Array.isArray(value) && value.every((item) => typeof item === 'number')) {
    return bytesField(7, encodeQueryVector(value))
  }
  if (typeof value === 'object') {
    if (typeof value.$int === 'string') return int64Field(3, BigInt(value.$int))
    if ('$uint' in value || '$decimal' in value) {
      throw new RedDBError(
        'UNSUPPORTED_PARAM',
        'exact uint and decimal params require a JSON body transport',
      )
    }
    if (typeof value.$bytes === 'string') return bytesField(6, Buffer.from(value.$bytes, 'base64'))
    if (typeof value.$float === 'string') return doubleField(4, Number(value.$float))
    if (value.$ts != null) return int64Field(9, BigInt(value.$ts))
    if (typeof value.$uuid === 'string') return bytesField(10, uuidBytes(value.$uuid))
    return stringField(8, JSON.stringify(value))
  }
  throw new RedDBError('UNSUPPORTED_PARAM', `cannot encode gRPC query parameter of type ${typeof value}`)
}

function encodeQueryVector(values) {
  const packed = new Uint8Array(values.length * 4)
  const view = new DataView(packed.buffer)
  values.forEach((value, index) => view.setFloat32(index * 4, Number(value), true))
  return bytesField(1, packed)
}

function encodeJsonCreateRequest(collection, payload) {
  return concat([
    stringField(1, collection ?? ''),
    stringField(2, JSON.stringify(serializeJsonValue(payload ?? {}))),
  ])
}

function encodeJsonBulkCreateRequest(collection, payloads) {
  const fields = [stringField(1, collection ?? '')]
  for (const payload of payloads ?? []) {
    fields.push(stringField(2, JSON.stringify(serializeJsonValue(payload ?? {}))))
  }
  return concat(fields)
}

function encodeDeleteEntityRequest(collection, id) {
  return concat([
    stringField(1, collection ?? ''),
    uint64Field(2, BigInt(id ?? 0)),
  ])
}

function decodeQueryReply(bytes) {
  const out = { ok: false, mode: '', statement: '', engine: '', columns: [], record_count: 0, result_json: '' }
  for (const field of readFields(bytes)) {
    if (field.no === 1 && field.wire === 0) out.ok = field.value !== 0n
    else if (field.no === 2 && field.wire === 2) out.mode = text(field.value)
    else if (field.no === 3 && field.wire === 2) out.statement = text(field.value)
    else if (field.no === 4 && field.wire === 2) out.engine = text(field.value)
    else if (field.no === 5 && field.wire === 2) out.columns.push(text(field.value))
    else if (field.no === 6 && field.wire === 0) out.record_count = safeBigIntToJs(field.value)
    else if (field.no === 7 && field.wire === 2) out.result_json = text(field.value)
  }
  return out
}

function decodeEntityReply(bytes) {
  const out = { ok: false, id: 0n, entity_json: '' }
  for (const field of readFields(bytes)) {
    if (field.no === 1 && field.wire === 0) out.ok = field.value !== 0n
    else if (field.no === 2 && field.wire === 0) out.id = field.value
    else if (field.no === 3 && field.wire === 2) out.entity_json = text(field.value)
  }
  return out
}

function decodeBulkEntityReply(bytes) {
  const out = { ok: false, count: 0n, items: [] }
  for (const field of readFields(bytes)) {
    if (field.no === 1 && field.wire === 0) out.ok = field.value !== 0n
    else if (field.no === 2 && field.wire === 0) out.count = field.value
    else if (field.no === 3 && field.wire === 2) out.items.push(decodeEntityReply(field.value))
  }
  return out
}

function decodeOperationReply(bytes) {
  const out = { ok: false, message: '' }
  for (const field of readFields(bytes)) {
    if (field.no === 1 && field.wire === 0) out.ok = field.value !== 0n
    else if (field.no === 2 && field.wire === 2) out.message = text(field.value)
  }
  return out
}

function decodeHealthReply(bytes) {
  const out = { healthy: false, state: '', checked_at_unix_ms: 0n }
  for (const field of readFields(bytes)) {
    if (field.no === 1 && field.wire === 0) out.healthy = field.value !== 0n
    else if (field.no === 2 && field.wire === 2) out.state = text(field.value)
    else if (field.no === 3 && field.wire === 0) out.checked_at_unix_ms = field.value
  }
  return out
}

function readFields(bytes) {
  let pos = 0
  const fields = []
  while (pos < bytes.length) {
    const key = readVarint(bytes, pos)
    pos = key.pos
    const no = Number(key.value >> 3n)
    const wire = Number(key.value & 0x07n)
    if (wire === 0) {
      const value = readVarint(bytes, pos)
      pos = value.pos
      fields.push({ no, wire, value: value.value })
    } else if (wire === 1) {
      fields.push({ no, wire, value: bytes.subarray(pos, pos + 8) })
      pos += 8
    } else if (wire === 2) {
      const len = readVarint(bytes, pos)
      pos = len.pos
      const end = pos + Number(len.value)
      fields.push({ no, wire, value: bytes.subarray(pos, end) })
      pos = end
    } else if (wire === 5) {
      fields.push({ no, wire, value: bytes.subarray(pos, pos + 4) })
      pos += 4
    } else {
      throw new RedDBError('GRPC_PROTOCOL', `unsupported protobuf wire type ${wire}`)
    }
  }
  return fields
}

function stringField(no, value) {
  return bytesField(no, new TextEncoder().encode(String(value)))
}

function bytesField(no, value) {
  const bytes = value instanceof Uint8Array ? value : new Uint8Array(value)
  return concat([varint((BigInt(no) << 3n) | 2n), varint(BigInt(bytes.length)), bytes])
}

function boolField(no, value) {
  return concat([varint((BigInt(no) << 3n) | 0n), varint(value ? 1n : 0n)])
}

function uint64Field(no, value) {
  return concat([varint((BigInt(no) << 3n) | 0n), varint(BigInt(value))])
}

function int64Field(no, value) {
  const raw = BigInt(value)
  if (raw < -(1n << 63n) || raw > (1n << 63n) - 1n) {
    throw new RedDBError('UNSUPPORTED_PARAM', 'integer param is outside i64 range')
  }
  return concat([varint((BigInt(no) << 3n) | 0n), varint(BigInt.asUintN(64, raw))])
}

function doubleField(no, value) {
  const bytes = new Uint8Array(8)
  new DataView(bytes.buffer).setFloat64(0, Number(value), true)
  return concat([varint((BigInt(no) << 3n) | 1n), bytes])
}

function varint(value) {
  let n = BigInt(value)
  const bytes = []
  while (n >= 0x80n) {
    bytes.push(Number((n & 0x7fn) | 0x80n))
    n >>= 7n
  }
  bytes.push(Number(n))
  return Uint8Array.from(bytes)
}

function readVarint(bytes, start) {
  let shift = 0n
  let value = 0n
  let pos = start
  while (pos < bytes.length) {
    const byte = BigInt(bytes[pos])
    pos += 1
    value |= (byte & 0x7fn) << shift
    if ((byte & 0x80n) === 0n) return { value, pos }
    shift += 7n
  }
  throw new RedDBError('GRPC_PROTOCOL', 'truncated protobuf varint')
}

function concat(parts) {
  const total = parts.reduce((sum, part) => sum + part.length, 0)
  const out = new Uint8Array(total)
  let pos = 0
  for (const part of parts) {
    out.set(part, pos)
    pos += part.length
  }
  return out
}

function text(bytes) {
  return new TextDecoder().decode(bytes)
}

function uuidBytes(value) {
  const hex = value.replace(/-/g, '')
  if (!/^[0-9a-f]{32}$/i.test(hex)) {
    throw new RedDBError('UNSUPPORTED_PARAM', `invalid UUID query parameter: ${value}`)
  }
  return Uint8Array.from(hex.match(/../g).map((pair) => Number.parseInt(pair, 16)))
}

function safeBigIntToJs(value) {
  if (
    value >= BigInt(Number.MIN_SAFE_INTEGER)
    && value <= BigInt(Number.MAX_SAFE_INTEGER)
  ) {
    return Number(value)
  }
  return value.toString()
}
