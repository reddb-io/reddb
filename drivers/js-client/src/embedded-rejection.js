/**
 * Embedded-URI rejection for the thin remote-only client.
 *
 * The `@reddb-io/client` package ships only the `red_client` binary,
 * which has no embedded engine. Any URI that asks for an in-memory
 * or file-backed database must be rejected at parse time with the
 * same wording the Rust `red_client` binary prints, so users get a
 * uniform error across language drivers and the CLI.
 *
 * Mirrors `is_embedded_uri` in
 * `crates/reddb-client/src/bin/red_client.rs` (the rejected forms):
 *
 *   "red://"          — bare red:// with no host
 *   "red:"            — degenerate
 *   "red:///"         — explicit empty path
 *   "red:///<path>"   — any red:// URL with a leading-slash path
 *   "red://:memory"   — SQLite-style alias
 *   "red://:memory:"  — SQLite-style alias
 *
 * The legacy `memory://`, `memory:`, and `file://<path>` schemes are
 * also rejected because they were always shorthand for the embedded
 * engine.
 */

import { RedDBError } from './protocol.js'

/** Wording is intentionally identical to the Rust binary stderr message. */
export const EMBEDDED_REJECTION_MESSAGE =
  'embedded schemes (memory:// / file://) are not supported.\n'
  + 'Use the full `red` binary for in-memory or file-backed engines.'

/**
 * Specialised error raised when an embedded URI is passed to the
 * thin client. Always carries `code === 'EmbeddedNotSupported'` and
 * the wording from `EMBEDDED_REJECTION_MESSAGE`, surfacing the same
 * actionable hint as the underlying Rust binary.
 */
export class EmbeddedNotSupported extends RedDBError {
  constructor(uri) {
    super('EmbeddedNotSupported', EMBEDDED_REJECTION_MESSAGE, { uri })
    this.name = 'EmbeddedNotSupported'
    this.uri = uri
  }
}

/**
 * Return true when `uri` selects the embedded engine.
 *
 * @param {string} uri
 * @returns {boolean}
 */
export function isEmbeddedUri(uri) {
  if (typeof uri !== 'string') return false
  const trimmed = uri.trim()
  if (
    trimmed === 'red://'
    || trimmed === 'red:'
    || trimmed === 'red:/'
    || trimmed === 'red:///'
    || trimmed === 'red://:memory'
    || trimmed === 'red://:memory:'
  ) {
    return true
  }
  // Any `red:///<path>` form is the embedded persistent engine.
  if (trimmed.startsWith('red:///')) return true
  // Legacy shorthands that always meant embedded.
  if (trimmed === 'memory://' || trimmed === 'memory:') return true
  if (trimmed.startsWith('file://')) return true
  return false
}

/**
 * Throws `EmbeddedNotSupported` if `uri` is an embedded shape.
 * Otherwise returns the trimmed URI for downstream consumption.
 *
 * @param {string} uri
 * @returns {string}
 */
export function rejectEmbeddedUri(uri) {
  if (typeof uri !== 'string' || uri.length === 0) {
    throw new TypeError(
      "connect() requires a URI string (e.g. 'red://localhost:5050' or 'grpc://host:5055')",
    )
  }
  if (isEmbeddedUri(uri)) {
    throw new EmbeddedNotSupported(uri)
  }
  return uri.trim()
}
