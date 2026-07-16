/**
 * Pure-JS NDJSON frame helpers shared by every text-framed transport.
 *
 *   - `classifyNdjsonFrame()` — parse one NDJSON line into a typed
 *     read-session frame (`descriptor` / `cursor` / `row` / `end`), throwing
 *     `RedDBError` on an `{error}` frame or unrecognised shape.
 *   - `splitLines()` — split a text buffer on `\n` into complete lines plus
 *     the trailing remainder, the building block for incremental NDJSON
 *     readers.
 *
 * Imports zero `node:` built-ins — no `node:stream`, no `Buffer`.
 */

import { RedDBError } from './errors.js'
import { normalizeExactNumbers } from './serialization.js'

/**
 * Parse an NDJSON line into a typed read-session frame, or `null` for a
 * blank line. Shared by the HTTP and any text-framed transport.
 * @param {string} line
 * @returns {{type:string,value:unknown}|null}
 */
export function classifyNdjsonFrame(line) {
  const trimmed = line.trim()
  if (trimmed.length === 0) return null
  let parsed
  try {
    parsed = JSON.parse(trimmed)
  } catch (err) {
    throw new RedDBError('STREAM_PROTOCOL', `stream frame is not JSON: ${err.message}`)
  }
  if (parsed && typeof parsed === 'object') {
    if ('descriptor' in parsed) return { type: 'descriptor', value: normalizeExactNumbers(parsed.descriptor) }
    if ('cursor' in parsed) return { type: 'cursor', value: normalizeExactNumbers(parsed.cursor) }
    if ('row' in parsed) return { type: 'row', value: normalizeExactNumbers(parsed.row) }
    if ('end' in parsed) return { type: 'end', value: normalizeExactNumbers(parsed.end) }
    if ('error' in parsed) {
      const e = parsed.error ?? {}
      throw new RedDBError(e.code || 'STREAM_ERROR', e.message || 'stream error', e)
    }
  }
  throw new RedDBError('STREAM_PROTOCOL', `unrecognised stream frame: ${trimmed.slice(0, 120)}`)
}

/**
 * Split a text buffer on `\n` into complete lines plus the trailing
 * remainder (the text after the last newline, which may be a partial
 * line awaiting more bytes). Pure — no I/O, no streams.
 * @param {string} buffer
 * @returns {{ lines: string[], rest: string }}
 */
export function splitLines(buffer) {
  const lines = []
  let nl
  while ((nl = buffer.indexOf('\n')) !== -1) {
    lines.push(buffer.slice(0, nl))
    buffer = buffer.slice(nl + 1)
  }
  return { lines, rest: buffer }
}
