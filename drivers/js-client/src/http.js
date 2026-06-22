/**
 * HTTP transport for the JS driver.
 *
 * Mirrors the public surface of `RpcClient` (call, close) but talks
 * straight to the RedDB HTTP server via fetch() — no binary spawn.
 * Each `RedDB` method is mapped to a REST endpoint; method names
 * stay identical to the JSON-RPC ones so `RedDB` doesn't need to
 * know which transport it's using.
 *
 * Endpoint mapping (server-side defined in src/server/routing.rs):
 *
 *   query / explain     → POST /query, POST /query/explain
 *   insert              → POST /collections/:name/rows
 *   bulk_insert         → POST /collections/:name/bulk/rows
 *   get                 → GET  /collections/:name/{id}      (entity scan + filter)
 *   delete              → DELETE /collections/:name/{id}
 *   health              → GET  /health
 *   version             → GET  /admin/version
 *   auth.login          → POST /auth/login
 *   auth.whoami         → GET  /auth/whoami
 *   auth.create_api_key → POST /auth/api-keys
 *   auth.revoke_api_key → DELETE /auth/api-keys/:key
 *   auth.change_password→ POST /auth/change-password
 *
 *   cache.get               → GET    /cache/ns/:ns/:key
 *   cache.put               → PUT    /cache/ns/:ns/:key
 *   cache.exists            → GET    /cache/ns/:ns/:key/exists
 *   cache.invalidate        → DELETE /cache/ns/:ns/:key
 *   cache.invalidate_prefix → DELETE /cache/ns/:ns?prefix=...
 *   cache.invalidate_tags   → DELETE /cache/ns/:ns/tags  (body: {tags})
 *   cache.flush_namespace   → POST   /admin/blob_cache/flush_namespace
 *
 * Auth: every request after construction carries `Authorization:
 * Bearer <token>` (when set). `setToken(token)` updates it in
 * place — used by the login flow that exchanges credentials for
 * a fresh bearer.
 */

import { RedDBError } from './protocol.js'
import { classifyNdjsonFrame, splitLines } from './core/ndjson.js'

export class HttpRpcClient {
  /**
   * @param {object} opts
   * @param {string} opts.baseUrl   Server origin, e.g. 'https://reddb.example.com:55555'
   * @param {string} [opts.token]   Bearer token / API key
   */
  constructor({ baseUrl, token }) {
    if (typeof baseUrl !== 'string' || baseUrl.length === 0) {
      throw new TypeError('HttpRpcClient: baseUrl required')
    }
    this.baseUrl = baseUrl.replace(/\/$/, '')
    this.token = token ?? null
  }

  setToken(token) {
    this.token = token
  }

  async close() {
    // HTTP is stateless — nothing to close.
  }

  /**
   * Generic RPC entry point. Routes the named method to the
   * corresponding HTTP endpoint and returns the parsed JSON body.
   */
  async call(method, params = {}) {
    const route = ROUTES[method]
    if (!route) {
      throw new RedDBError(
        'UNKNOWN_METHOD',
        `HTTP transport has no route for method '${method}'`,
      )
    }
    const { url, init } = route(this.baseUrl, params)
    const response = await fetch(url, this.attachAuth(init))
    return parseResponse(response)
  }

  attachAuth(init) {
    const headers = new Headers(init.headers || {})
    if (this.token && !headers.has('authorization')) {
      headers.set('authorization', `Bearer ${this.token}`)
    }
    if (init.body && !headers.has('content-type')) {
      headers.set('content-type', 'application/json')
    }
    return { ...init, headers }
  }

  authHeaders(extra = {}) {
    const headers = new Headers(extra)
    if (this.token && !headers.has('authorization')) {
      headers.set('authorization', `Bearer ${this.token}`)
    }
    return headers
  }

  /**
   * Open a streaming read against `POST /query/stream`. Returns an async
   * iterable of typed frames (see streaming.js) plus a `cancel(reason)`
   * that aborts the underlying fetch. A non-streaming refusal (e.g. a
   * non-read-only statement) is surfaced as a rejected `RedDBError`
   * before any frame is yielded, so callers can tell "never accepted"
   * from a mid-stream failure.
   *
   * @param {{ sql?: string, cursor?: string, signal?: AbortSignal }} opts
   */
  async streamSelect({ sql, cursor, signal } = {}) {
    const controller = new AbortController()
    linkSignal(signal, controller)
    const body = cursor != null ? { cursor } : { query: sql }
    const response = await fetch(`${this.baseUrl}/query/stream`, {
      method: 'POST',
      headers: this.authHeaders({ 'content-type': 'application/json' }),
      body: JSON.stringify(body),
      signal: controller.signal,
    })
    if (!response.ok) {
      await throwHttpStreamRefusal(response)
    }
    const reader = response.body?.getReader()
    if (!reader) {
      throw new RedDBError('STREAM_PROTOCOL', 'streaming response had no body')
    }
    return {
      [Symbol.asyncIterator]() {
        return ndjsonFrameIterator(reader, controller)
      },
      async cancel() {
        controller.abort()
        try {
          await reader.cancel()
        } catch {
          // best-effort
        }
      },
    }
  }

  /**
   * Open a streaming write against `POST /streams/input`. The request
   * body is an NDJSON stream: an `open` frame (target + columns), then
   * one `row` frame per record. Backpressure flows through the request
   * body's writer. The terminal envelope is returned by `close()`.
   *
   * @param {{ target: string, columns?: string[], signal?: AbortSignal }} opts
   */
  async streamInput({ target, columns, signal } = {}) {
    const controller = new AbortController()
    linkSignal(signal, controller)
    const transform = new TransformStream()
    const writer = transform.writable.getWriter()
    const fetchPromise = fetch(`${this.baseUrl}/streams/input`, {
      method: 'POST',
      headers: this.authHeaders({ 'content-type': 'application/x-ndjson' }),
      body: transform.readable,
      duplex: 'half',
      signal: controller.signal,
    })
    // Surface a connection/refusal failure on the write path too, rather
    // than leaving the promise unhandled if the caller never calls close().
    fetchPromise.catch(() => {})

    let opened = false
    let cols = Array.isArray(columns) && columns.length > 0 ? columns.slice() : null
    const encodeLine = (obj) => writer.write(`${JSON.stringify(obj)}\n`)
    const ensureOpen = async (row) => {
      if (opened) return
      if (!cols) {
        cols = row && typeof row === 'object' ? Object.keys(row) : null
      }
      if (!cols || cols.length === 0) {
        throw new RedDBError(
          'INVALID_STREAM_COLUMNS',
          'inputStream() needs a non-empty column set — pass { columns } or write at least one object row',
        )
      }
      await writer.write(`${JSON.stringify({ open: { target, columns: cols } })}\n`)
      opened = true
    }

    return {
      async write(row) {
        await ensureOpen(row)
        await writer.ready
        await encodeLine({ row })
      },
      async close() {
        await ensureOpen(null)
        await writer.close()
        const response = await fetchPromise
        return await readInputTerminal(response)
      },
      async cancel(reason) {
        controller.abort()
        try {
          await writer.abort(reason)
        } catch {
          // best-effort — the abort above already tore the request down.
        }
      },
    }
  }
}

function linkSignal(signal, controller) {
  if (!signal) return
  if (signal.aborted) {
    controller.abort(signal.reason)
    return
  }
  signal.addEventListener('abort', () => controller.abort(signal.reason), { once: true })
}

async function throwHttpStreamRefusal(response) {
  const text = await response.text().catch(() => '')
  let body = null
  if (text) {
    try {
      body = JSON.parse(text)
    } catch {
      body = { raw: text }
    }
  }
  const code = body?.code || body?.error_code || `HTTP_${response.status}`
  const message =
    body?.error || body?.message || `stream refused with status ${response.status}`
  throw new RedDBError(code, message, body)
}

/** Async iterator over NDJSON frames from a web ReadableStream reader. */
async function* ndjsonFrameIterator(reader, controller) {
  const decoder = new TextDecoder()
  let buffer = ''
  try {
    for (;;) {
      const { value, done } = await reader.read()
      if (done) break
      buffer += decoder.decode(value, { stream: true })
      const { lines, rest } = splitLines(buffer)
      buffer = rest
      for (const line of lines) {
        const frame = classifyNdjsonFrame(line)
        if (frame) yield frame
      }
    }
    const tail = buffer + decoder.decode()
    const frame = classifyNdjsonFrame(tail)
    if (frame) yield frame
  } finally {
    controller.abort()
  }
}

/** Read the input-stream response body and return its terminal envelope. */
async function readInputTerminal(response) {
  const text = await response.text()
  if (!response.ok) {
    let body = null
    try {
      body = text ? JSON.parse(text) : null
    } catch {
      body = { raw: text }
    }
    const code = body?.code || body?.error_code || `HTTP_${response.status}`
    const message = body?.error || body?.message || `input stream failed (${response.status})`
    throw new RedDBError(code, message, body)
  }
  const lines = text.split('\n').map((l) => l.trim()).filter((l) => l.length > 0)
  let end = null
  for (const line of lines) {
    const frame = classifyNdjsonFrame(line) // throws RedDBError on an {error} frame
    if (frame && frame.type === 'end') end = frame.value
  }
  if (!end) {
    throw new RedDBError('STREAM_PROTOCOL', 'input stream closed without a terminal end envelope')
  }
  return end
}

async function parseResponse(response) {
  const text = await response.text()
  let body = null
  if (text) {
    try {
      body = JSON.parse(text)
    } catch {
      body = { raw: text }
    }
  }
  if (!response.ok) {
    const code = body?.error_code || `HTTP_${response.status}`
    const message = body?.error || body?.message || `request failed with status ${response.status}`
    throw new RedDBError(code, message, body)
  }
  // RedDB envelope is `{ ok, result, error? }` for some endpoints
  // and bare JSON for others. Unwrap the envelope when present.
  if (body && typeof body === 'object' && 'ok' in body) {
    if (body.ok === false) {
      const code = body.error_code || 'RPC_ERROR'
      throw new RedDBError(code, body.error || 'unknown error', body)
    }
    return body.result ?? body
  }
  return body
}

// ---------------------------------------------------------------------------
// Method → HTTP route mapping
// ---------------------------------------------------------------------------

const ROUTES = {
  health: (base) => ({ url: `${base}/health`, init: { method: 'GET' } }),
  version: (base) => ({ url: `${base}/admin/version`, init: { method: 'GET' } }),
  query: (base, { sql, params }) => ({
    url: `${base}/query`,
    init: {
      method: 'POST',
      body: JSON.stringify(
        Array.isArray(params) ? { query: sql, params } : { query: sql },
      ),
    },
  }),
  insert: (base, { collection, payload }) => ({
    url: `${base}/collections/${encodeURIComponent(collection)}/rows`,
    init: { method: 'POST', body: JSON.stringify(payload) },
  }),
  bulk_insert: (base, { collection, payloads }) => ({
    url: `${base}/collections/${encodeURIComponent(collection)}/bulk/rows`,
    init: { method: 'POST', body: JSON.stringify({ rows: payloads }) },
  }),
  get: (base, { collection, id }) => ({
    url: `${base}/collections/${encodeURIComponent(collection)}/${encodeURIComponent(id)}`,
    init: { method: 'GET' },
  }),
  delete: (base, { collection, id }) => ({
    url: `${base}/collections/${encodeURIComponent(collection)}/${encodeURIComponent(id)}`,
    init: { method: 'DELETE' },
  }),
  'auth.login': (base, { username, password }) => ({
    url: `${base}/auth/login`,
    init: { method: 'POST', body: JSON.stringify({ username, password }) },
  }),
  'auth.whoami': (base) => ({
    url: `${base}/auth/whoami`,
    init: { method: 'GET' },
  }),
  'auth.create_api_key': (base, params = {}) => ({
    url: `${base}/auth/api-keys`,
    init: { method: 'POST', body: JSON.stringify(params) },
  }),
  'auth.revoke_api_key': (base, { key }) => ({
    url: `${base}/auth/api-keys/${encodeURIComponent(key)}`,
    init: { method: 'DELETE' },
  }),
  'auth.change_password': (base, { current_password, new_password }) => ({
    url: `${base}/auth/change-password`,
    init: {
      method: 'POST',
      body: JSON.stringify({
        current_password,
        new_password,
      }),
    },
  }),
  'cache.get': (base, { namespace, key }) => ({
    url: `${base}/cache/ns/${encodeURIComponent(namespace)}/${encodeURIComponent(key)}`,
    init: { method: 'GET' },
  }),
  'cache.put': (base, { namespace, key, value, ttl_ms, tags, policy }) => ({
    url: `${base}/cache/ns/${encodeURIComponent(namespace)}/${encodeURIComponent(key)}`,
    init: { method: 'PUT', body: JSON.stringify({ value, ttl_ms, tags, policy }) },
  }),
  'cache.exists': (base, { namespace, key }) => ({
    url: `${base}/cache/ns/${encodeURIComponent(namespace)}/${encodeURIComponent(key)}/exists`,
    init: { method: 'GET' },
  }),
  'cache.invalidate': (base, { namespace, key }) => ({
    url: `${base}/cache/ns/${encodeURIComponent(namespace)}/${encodeURIComponent(key)}`,
    init: { method: 'DELETE' },
  }),
  'cache.invalidate_prefix': (base, { namespace, prefix }) => ({
    url: `${base}/cache/ns/${encodeURIComponent(namespace)}?prefix=${encodeURIComponent(prefix)}`,
    init: { method: 'DELETE' },
  }),
  'cache.invalidate_tags': (base, { namespace, tags }) => ({
    url: `${base}/cache/ns/${encodeURIComponent(namespace)}/tags`,
    init: { method: 'DELETE', body: JSON.stringify({ tags }) },
  }),
  'cache.flush_namespace': (base, { namespace }) => ({
    url: `${base}/admin/blob_cache/flush_namespace`,
    init: { method: 'POST', body: JSON.stringify({ namespace }) },
  }),
}
