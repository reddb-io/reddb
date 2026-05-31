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

export function serializeParam(value) {
  assertSupportedParam(value)
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

export function assertSupportedParam(value) {
  if (value == null) return
  if (
    typeof value === 'boolean'
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
