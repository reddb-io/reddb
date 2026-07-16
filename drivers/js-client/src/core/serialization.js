/**
 * SQL query-parameter serialization — the wire encoding shared by every
 * transport. Pure JS: value encoding, base64, UUID, Date, typed-array,
 * and NaN/Infinity handling. Imports zero `node:` built-ins.
 *
 * `Buffer` is referenced only behind a `typeof Buffer !== 'undefined'`
 * guard so the module loads unchanged on runtimes that have no `Buffer`
 * (browsers, Deno without the node shim).
 */

import { RedDBError } from './errors.js'

const MIN_I64 = -(1n << 63n)
const MAX_I64 = (1n << 63n) - 1n
const MAX_U64 = (1n << 64n) - 1n

export function serializeParam(value) {
  assertSupportedParam(value)
  if (typeof value === 'bigint') {
    return exactIntegerEnvelope(value)
  }
  if (value instanceof Float32Array || value instanceof Float64Array) {
    return Array.from(value)
  }
  if (value instanceof Date) {
    return { $ts: String(BigInt(value.getTime()) * 1_000_000n) }
  }
  if (value instanceof Uint8Array || (typeof Buffer !== 'undefined' && value instanceof Buffer)) {
    return { $bytes: bytesToBase64(value) }
  }
  if (typeof value === 'number' && !Number.isFinite(value)) {
    if (Number.isNaN(value)) return { $float: 'NaN' }
    return { $float: value > 0 ? 'Infinity' : '-Infinity' }
  }
  if (typeof value === 'string' && isUuidString(value)) {
    return { $uuid: value }
  }
  return value
}

export function serializeJsonValue(value) {
  if (typeof value === 'bigint') {
    return exactIntegerEnvelope(value)
  }
  if (Array.isArray(value)) {
    return value.map(serializeJsonValue)
  }
  if (value && typeof value === 'object' && Object.getPrototypeOf(value) === Object.prototype) {
    if ('$number' in value || '$decimalText' in value) {
      throw new RedDBError('UNSUPPORTED_EXACT_NUMBER', 'superseded exact-number envelope')
    }
    const out = {}
    for (const [key, item] of Object.entries(value)) out[key] = serializeJsonValue(item)
    return out
  }
  return value
}

export function normalizeExactNumbers(value) {
  if (Array.isArray(value)) return value.map(normalizeExactNumbers)
  if (value && typeof value === 'object') {
    const keys = Object.keys(value)
    if (keys.length === 1) {
      if (typeof value.$int === 'string' || typeof value.$uint === 'string') {
        return BigInt(value.$int ?? value.$uint)
      }
      if (typeof value.$decimal === 'string') return value.$decimal
      if ('$number' in value || '$decimalText' in value) {
        throw new RedDBError('UNSUPPORTED_EXACT_NUMBER', 'superseded exact-number envelope')
      }
    }
    const out = {}
    for (const [key, item] of Object.entries(value)) out[key] = normalizeExactNumbers(item)
    return out
  }
  return value
}

function exactIntegerEnvelope(value) {
  if (value >= MIN_I64 && value <= MAX_I64) {
    return { $int: value.toString() }
  }
  if (value >= 0n && value <= MAX_U64) {
    return { $uint: value.toString() }
  }
  throw new RedDBError('UNSUPPORTED_PARAM', 'integer value is outside i64/u64 range')
}

export function assertSupportedParam(value) {
  if (value == null) return
  if (
    typeof value === 'boolean'
    || typeof value === 'bigint'
    || typeof value === 'number'
    || typeof value === 'string'
  ) {
    return
  }
  if (value instanceof Date) {
    if (Number.isNaN(value.getTime())) {
      throw new RedDBError('UNSUPPORTED_PARAM', 'cannot encode invalid Date query parameter')
    }
    return
  }
  if (
    value instanceof Uint8Array
    || value instanceof Float32Array
    || value instanceof Float64Array
    || (typeof Buffer !== 'undefined' && value instanceof Buffer)
  ) {
    return
  }
  if (Array.isArray(value)) {
    if (value.every((item) => typeof item === 'number')) return
    throw new RedDBError(
      'UNSUPPORTED_PARAM',
      'array query parameters must contain only numbers',
    )
  }
  if (typeof value === 'object' && Object.getPrototypeOf(value) === Object.prototype) {
    return
  }
  throw new RedDBError(
    'UNSUPPORTED_PARAM',
    `cannot encode query parameter of type ${typeof value}`,
  )
}

export function normalizeQueryParams(args) {
  if (args.length === 0) return null
  if (args.length === 1 && Array.isArray(args[0])) return args[0].map(serializeParam)
  return args.map(serializeParam)
}

export function bytesToBase64(value) {
  const bytes = value instanceof Uint8Array
    ? value
    : new Uint8Array(value.buffer, value.byteOffset, value.byteLength)
  if (typeof Buffer !== 'undefined') {
    return Buffer.from(bytes.buffer, bytes.byteOffset, bytes.byteLength).toString('base64')
  }
  let text = ''
  for (const byte of bytes) text += String.fromCharCode(byte)
  // eslint-disable-next-line no-undef
  return btoa(text)
}

export function isUuidString(value) {
  return /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value)
}
