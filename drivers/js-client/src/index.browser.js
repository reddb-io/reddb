/**
 * @reddb-io/client — browser entrypoint.
 *
 * The browser counterpart to `./index.js`. It builds `connect()` on the
 * transport-agnostic core (`./core/index.js`), dispatching only the
 * browser-reachable wires:
 *
 *   - 'http://host:port'   — HTTP JSON-RPC over `fetch`
 *   - 'https://host:port'  — HTTPS JSON-RPC over `fetch`
 *   - 'ws(s)://host:port'  — RedWire over binary WebSocket
 *
 * Streaming is the Web-streams implementation (`./streaming-web.js`), so the
 * full transport-agnostic surface works client-side: query, execute, insert,
 * bulkInsert, transactions, the kv/documents/queue/cache/config/vault clients,
 * the typed query builder, and streaming (`stream()` / `inputStream()`).
 *
 * This module imports **neither** the gRPC transport (`./grpc.js`, which pulls
 * `node:http2`) **nor** the RedWire transport (`./redwire.js`) **nor** the
 * `node:stream` streaming impl (`./streaming.js`). No `node:` built-in enters
 * the browser bundle graph through it. A portability guard test
 * (`test/browser-portability.test.mjs`) is the regression net for that.
 *
 * Schemes that need a raw TCP socket or HTTP/2 — `grpc://`, `grpcs://`,
 * `red://`, `reds://`, and `pg` — are not reachable from a browser sandbox.
 * `connect()` rejects them with an actionable error (see
 * `BROWSER_TRANSPORT_UNSUPPORTED` below) instead of crashing the bundler or
 * runtime. Embedded URIs (`memory://`, `file://`, `red:///path`) are rejected
 * with the same wording as the Node entry.
 */

import {
  RedDBError,
  RedDB as CoreRedDB,
  Collection,
  EmbeddedNotSupported,
  EMBEDDED_REJECTION_MESSAGE,
  isEmbeddedUri,
  rejectEmbeddedUri,
  parseUri,
  login,
  mergeAuthFromUri,
} from './core/index.js'
import { HttpRpcClient } from './http.js'
import { connectRedwireWs, REDWIRE_WS_PATH } from './redwire-ws.js'
import { createSelectStream, createInputStream } from './streaming-web.js'

export { RedDBError, EmbeddedNotSupported, EMBEDDED_REJECTION_MESSAGE, isEmbeddedUri }
export { RowReadable, RowWritable } from './streaming-web.js'
export { CacheClient } from './cache.js'
export { KvClient } from './kv.js'
export { QueueClient } from './queue.js'
export { DocumentClient } from './documents.js'
export { ConfigClient } from './config.js'
export { VaultClient } from './vault.js'
export { TypedQueryBuilder } from './db-helpers.js'
export { parseUri, deriveLoginUrl } from './url.js'
export { login }

// The Web-streams streaming implementation, injected into the core `RedDB` so
// its `stream()` / `inputStream()` return Web-streams-backed row wrappers. The
// core itself never statically references Web (or `node:`) streams.
const WEB_STREAMING = { createSelectStream, createInputStream }

/**
 * Shared wording for schemes a browser sandbox cannot reach: they need a raw
 * TCP socket (`red(s)://`, `pg`) or HTTP/2 (`grpc(s)://`), neither of which a
 * browser exposes to JavaScript. The remedy is the same in every case — point
 * the client at an HTTP(S) endpoint or gateway in front of the server.
 */
function browserTransportError(scheme) {
  return new RedDBError(
    'BROWSER_TRANSPORT_UNSUPPORTED',
    `'${scheme}' connections are not available in the browser: the `
      + `browser sandbox exposes no raw TCP socket (red://, reds://, pg) or `
      + `HTTP/2 client (grpc://, grpcs://) to JavaScript. Connect to an `
      + `HTTP(S) endpoint or RedWire WebSocket endpoint instead — e.g. `
      + `'http://host:port' / 'https://host:port' / 'wss://host' — by running `
      + `RedDB's HTTP JSON-RPC listener, its WebSocket edge, or an HTTP gateway `
      + `in front of the server.`,
  )
}

/**
 * Connect to a remote RedDB instance from a browser.
 *
 * @param {string} uri Connection URI. `http(s)://` and `ws(s)://` are
 *   reachable from a browser; other schemes raise `BROWSER_TRANSPORT_UNSUPPORTED`.
 * @param {object} [options]
 * @param {object} [options.auth] Authentication credentials.
 * @param {string} [options.auth.token] Bearer / API-key token.
 * @param {string} [options.auth.apiKey] Alias for `token`.
 * @param {string} [options.auth.username] Username for password login.
 * @param {string} [options.auth.password] Password for password login.
 * @param {string} [options.auth.loginUrl] Override URL for the password
 *   exchange (defaults to deriving `/auth/login` from `uri`).
 * @returns {Promise<RedDB>}
 */
export async function connect(uri, options = {}) {
  // Reject embedded shapes upfront with the same wording the Node entry and
  // the Rust binary use, before the URL parser would map them to kind=embedded.
  rejectEmbeddedUri(uri)

  const parsed = parseUri(uri)

  // Belt-and-braces: if the parser still produced an embedded kind, reject it.
  if (parsed.kind === 'embedded') {
    throw new EmbeddedNotSupported(uri)
  }

  if (parsed.kind === 'http' || parsed.kind === 'https') {
    const merged = mergeAuthFromUri(parsed, options.auth)
    const baseUrl = `${parsed.kind}://${parsed.host}:${parsed.port}`
    let token = merged.token
    if (!token && merged.username && merged.password) {
      const loginUrl = merged.loginUrl ?? `${baseUrl}/auth/login`
      const session = await login(loginUrl, {
        username: merged.username,
        password: merged.password,
      })
      token = session.token
    }
    const client = new HttpRpcClient({ baseUrl, token })
    await client.call('query', { sql: 'SELECT 1' })
    return new RedDB(client)
  }

  // RedWire-over-binary-WebSocket (#937, ADR 0036): the browser speaks the
  // same multiplexed binary protocol as the native drivers, tunneled over
  // a WSS the sandbox can open. The TLS edge enforces the Origin allowlist
  // and WSS-only on its side; here we just open the socket and run the
  // standard RedWire handshake over it.
  if (parsed.kind === 'redws' || parsed.kind === 'redwss') {
    const merged = mergeAuthFromUri(parsed, options.auth)
    let token = merged.token
    if (!token && merged.username && merged.password) {
      const httpScheme = parsed.tls === false ? 'http' : 'https'
      const loginUrl = merged.loginUrl ?? `${httpScheme}://${parsed.host}:${parsed.port}/auth/login`
      const session = await login(loginUrl, {
        username: merged.username,
        password: merged.password,
      })
      token = session.token
    }
    const auth = token ? { kind: 'bearer', token } : { kind: 'anonymous' }
    const wsScheme = parsed.tls === false ? 'ws' : 'wss'
    const url = `${wsScheme}://${parsed.host}:${parsed.port}${REDWIRE_WS_PATH}`
    const client = await connectRedwireWs({ url, auth })
    return new RedDB(client)
  }

  if (
    parsed.kind === 'grpc'
    || parsed.kind === 'grpcs'
    || parsed.kind === 'red'
    || parsed.kind === 'reds'
    || parsed.kind === 'pg'
  ) {
    throw browserTransportError(parsed.kind)
  }

  throw new RedDBError(
    'UNSUPPORTED_KIND',
    `internal: parsed kind '${parsed.kind}' has no browser transport`,
  )
}

/**
 * Browser connection handle. The full request-shaping surface lives in the
 * transport-agnostic core `RedDB`; this subclass exists only to inject the
 * Web-streams streaming implementation so `stream()` / `inputStream()` return
 * Web-streams-backed row wrappers. The public surface — every method, the
 * `kv`/`config`/`vault` factory shapes, the `cache`/`queue`/`documents`
 * clients — is the core's, unchanged.
 */
export class RedDB extends CoreRedDB {
  /** @param {HttpRpcClient} client */
  constructor(client) {
    super(client, WEB_STREAMING)
  }
}

export { Collection }
