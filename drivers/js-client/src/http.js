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
 * Auth: every request after construction carries `Authorization:
 * Bearer <token>` (when set). `setToken(token)` updates it in
 * place — used by the login flow that exchanges credentials for
 * a fresh bearer.
 */

import { RedDBError } from './protocol.js'

export class HttpRpcClient {
  /**
   * @param {object} opts
   * @param {string} opts.baseUrl   Server origin, e.g. 'https://reddb.example.com:8443'
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
  query: (base, { sql }) => ({
    url: `${base}/query`,
    init: { method: 'POST', body: JSON.stringify({ query: sql }) },
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
}
