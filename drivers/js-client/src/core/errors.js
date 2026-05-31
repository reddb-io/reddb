/**
 * Transport-agnostic error type shared by the whole driver.
 *
 * Lives in the core so every core module (serialization, auth, NDJSON
 * helpers, the connection handle) can raise it without reaching for a
 * transport-specific module. Imports zero `node:` built-ins.
 */

/**
 * RedDB-shaped error. Drivers in other languages should expose an
 * equivalent class with the same `code` field.
 */
export class RedDBError extends Error {
  constructor(code, message, data) {
    super(message)
    this.name = 'RedDBError'
    this.code = code
    this.data = data ?? null
  }
}
