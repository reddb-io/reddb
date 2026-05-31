/**
 * @reddb-io/client — thin remote-only RedDB driver.
 *
 * Public API:
 *   import { connect } from '@reddb-io/client'
 *   const db = await connect('red://reddb.example.com:5050')
 *   const result = await db.query('SELECT * FROM users LIMIT 10')
 *   await db.close()
 *
 * Accepted URIs:
 *   - 'red://host:port'        — RedWire TCP (default)
 *   - 'reds://host:port'       — RedWire over TLS
 *   - 'grpc://host:port'       — gRPC
 *   - 'grpcs://host:port'      — gRPC over TLS
 *   - 'http://host:port'       — HTTP JSON-RPC
 *   - 'https://host:port'      — HTTPS JSON-RPC
 *
 * Rejected URIs (use @reddb-io/sdk for these):
 *   - 'memory://', 'memory:'   — in-memory embedded engine
 *   - 'file:///abs/path'       — file-backed embedded engine
 *   - 'red://', 'red:///path'  — same shapes via the unified scheme
 *   - 'red://:memory[:]'       — SQLite-style embedded alias
 *
 * The thin `red_client` binary does not bundle the storage engine —
 * the package is roughly 10x smaller than `@reddb-io/sdk`. If you
 * need an embedded engine, install `@reddb-io/sdk` instead.
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
  deriveLoginUrl,
  login,
  mergeAuthFromUri,
} from './core/index.js'
import { HttpRpcClient } from './http.js'
import { GrpcRpcClient } from './grpc.js'
import { connectRedwire } from './redwire.js'
import { createSelectStream, createInputStream } from './streaming.js'

export { RedDBError, EmbeddedNotSupported, EMBEDDED_REJECTION_MESSAGE, isEmbeddedUri }
export { splitNdjson, RowReadable, RowWritable } from './streaming.js'
export { CacheClient } from './cache.js'
export { KvClient } from './kv.js'
export { QueueClient } from './queue.js'
export { DocumentClient } from './documents.js'
export { ConfigClient } from './config.js'
export { VaultClient } from './vault.js'
export { TypedQueryBuilder } from './db-helpers.js'
export { parseUri, deriveLoginUrl } from './url.js'
export { login }

// The `node:stream`-based streaming implementation, injected into the core
// `RedDB` so its `stream()` / `inputStream()` return Node streams. The core
// itself never statically references `node:stream`.
const NODE_STREAMING = { createSelectStream, createInputStream }

/**
 * Connect to a remote RedDB instance.
 *
 * @param {string} uri Connection URI. See module docstring for accepted schemes.
 * @param {object} [options]
 * @param {object} [options.auth] Authentication credentials.
 * @param {string} [options.auth.token] Bearer / API-key token.
 * @param {string} [options.auth.apiKey] Alias for `token`.
 * @param {string} [options.auth.username] Username for password login.
 * @param {string} [options.auth.password] Password for password login.
 * @param {string} [options.auth.loginUrl] Override URL for the password
 *   exchange (defaults to deriving `/auth/login` from `uri`).
 * @param {object} [options.tls] TLS / mTLS options for redwire(s)://.
 * @returns {Promise<RedDB>}
 */
export async function connect(uri, options = {}) {
  // Reject embedded shapes upfront with the same wording the Rust
  // binary uses, before the URL parser would map them to kind=embedded.
  rejectEmbeddedUri(uri)

  const parsed = parseUri(uri)

  // Belt-and-braces: if the parser still produced an embedded kind
  // (e.g. via a URI shape we forgot to enumerate above), reject it.
  if (parsed.kind === 'embedded') {
    throw new EmbeddedNotSupported(uri)
  }

  const merged = mergeAuthFromUri(parsed, options.auth)

  if (parsed.kind === 'http' || parsed.kind === 'https') {
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

  if (parsed.kind === 'grpc' || parsed.kind === 'grpcs') {
    let token = merged.token
    if (!token && merged.username && merged.password) {
      const loginUrl = merged.loginUrl ?? deriveLoginUrl(parsed)
      const session = await login(loginUrl, {
        username: merged.username,
        password: merged.password,
      })
      token = session.token
    }
    const scheme = parsed.kind === 'grpcs' ? 'https' : 'http'
    const client = new GrpcRpcClient({
      baseUrl: `${scheme}://${parsed.host}:${parsed.port}`,
      token,
    })
    return new RedDB(client)
  }

  if (parsed.kind === 'red' || parsed.kind === 'reds') {
    let token = merged.token
    if (!token && merged.username && merged.password) {
      const loginUrl = merged.loginUrl ?? deriveLoginUrl(parsed)
      const session = await login(loginUrl, {
        username: merged.username,
        password: merged.password,
      })
      token = session.token
    }
    const auth = token ? { kind: 'bearer', token } : { kind: 'anonymous' }
    const tls = buildTlsOpts(parsed, options.tls)
    const client = await connectRedwire({
      host: parsed.host,
      port: parsed.port,
      auth,
      ...(tls ? { tls } : {}),
    })
    return new RedDB(client)
  }

  if (parsed.kind === 'pg') {
    throw new RedDBError(
      'PG_TRANSPORT_NOT_WIRED',
      "PostgreSQL wire (proto=pg) requires a node-pg-style client; "
        + "the JS thin client doesn't bundle one. Use a separate `pg` "
        + 'package against the same host:port.',
    )
  }

  throw new RedDBError(
    'UNSUPPORTED_KIND',
    `internal: parsed kind '${parsed.kind}' has no transport`,
  )
}

/**
 * Resolve TLS options for a redwire(s) connection. Source order:
 *   1. caller-supplied `options.tls` object.
 *   2. `parsed.kind === 'reds' | 'grpcs'`.
 *   3. `?tls=true` URL param.
 *   4. `?ca=`, `?cert=`, `?key=`, `?servername=`, `?rejectUnauthorized=false`
 *      URL params (path or PEM string).
 */
function buildTlsOpts(parsed, callerTls) {
  if (callerTls && typeof callerTls === 'object') {
    return callerTls
  }
  const params = parsed.params
  const wantsTls =
    parsed.kind === 'reds'
    || parsed.kind === 'grpcs'
    || params?.get?.('tls') === 'true'
    || params?.get?.('tls') === '1'
  if (!wantsTls) return null
  return {
    ca: params?.get?.('ca') ?? undefined,
    cert: params?.get?.('cert') ?? undefined,
    key: params?.get?.('key') ?? undefined,
    servername: params?.get?.('servername') ?? undefined,
    rejectUnauthorized:
      params?.get?.('rejectUnauthorized') === 'false' ? false : true,
  }
}

/**
 * Node connection handle. The full request-shaping surface lives in the
 * transport-agnostic core `RedDB`; this subclass exists only to inject the
 * `node:stream`-based streaming implementation so `stream()` / `inputStream()`
 * return Node streams. The public surface — every method, the `kv`/`config`/
 * `vault` factory shapes, the `cache`/`queue`/`documents` clients — is the
 * core's, unchanged.
 *
 * The `Collection` handle (`db.collection(name)`) is re-exported from the
 * core verbatim.
 */
export class RedDB extends CoreRedDB {
  /** @param {HttpRpcClient | import('./redwire.js').RedWireClient} client */
  constructor(client) {
    super(client, NODE_STREAMING)
  }
}

export { Collection }
