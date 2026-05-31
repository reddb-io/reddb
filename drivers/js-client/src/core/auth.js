/**
 * Authentication helpers shared across transports.
 *
 *   - `login()`            — username/password → bearer-token exchange over
 *                            the server's `POST /auth/login` HTTP endpoint.
 *   - `mergeAuthFromUri()` — fold credentials parsed from the connection
 *                            URI together with caller-supplied `options.auth`.
 *
 * Uses the global `fetch`; imports zero `node:` built-ins.
 */

import { RedDBError } from './errors.js'

/**
 * Exchange username + password for a bearer token via the server's
 * `POST /auth/login` HTTP endpoint. Same flow used by `connect()` when
 * the caller passes `auth: { username, password }`.
 *
 * @param {string} loginUrl Full URL of the server's auth endpoint.
 * @param {{ username: string, password: string }} credentials
 * @returns {Promise<{ token: string, username: string, role: string, expires_at: number }>}
 */
export async function login(loginUrl, { username, password }) {
  if (typeof loginUrl !== 'string' || !loginUrl.startsWith('http')) {
    throw new TypeError("login() requires an http(s):// URL pointing at /auth/login")
  }
  if (typeof username !== 'string' || username.length === 0) {
    throw new TypeError('login() requires a non-empty username')
  }
  if (typeof password !== 'string' || password.length === 0) {
    throw new TypeError('login() requires a non-empty password')
  }
  const response = await fetch(loginUrl, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ username, password }),
  })
  const body = await response.json().catch(() => ({}))
  if (!response.ok || body.ok === false) {
    const code = body.error_code || `HTTP_${response.status}`
    const message = body.error || `auth/login returned ${response.status}`
    throw new RedDBError(code, message, body)
  }
  if (typeof body.token !== 'string') {
    throw new RedDBError(
      'AUTH_LOGIN_BAD_RESPONSE',
      'auth/login response missing string token',
      body,
    )
  }
  return body
}

/**
 * Fold credentials carried on the parsed URI together with the
 * caller-supplied `options.auth`. `options.auth` wins on every field it
 * sets; URI-derived values are the fallback. Returns a normalised
 * `{ token, username, password, loginUrl }` shape.
 *
 * @param {object} parsed result of `parseUri()`.
 * @param {object} [optionAuth] caller-supplied `options.auth`.
 */
export function mergeAuthFromUri(parsed, optionAuth) {
  const out = {
    token: parsed.token ?? parsed.apiKey ?? null,
    username: parsed.username ?? null,
    password: parsed.password ?? null,
    loginUrl: parsed.loginUrl ?? null,
  }
  if (optionAuth == null) return out
  if (typeof optionAuth !== 'object') {
    throw new TypeError('options.auth must be an object')
  }
  if (optionAuth.token != null) {
    if (typeof optionAuth.token !== 'string' || optionAuth.token.length === 0) {
      throw new TypeError('options.auth.token must be a non-empty string')
    }
    out.token = optionAuth.token
  }
  if (optionAuth.apiKey != null) {
    if (typeof optionAuth.apiKey !== 'string' || optionAuth.apiKey.length === 0) {
      throw new TypeError('options.auth.apiKey must be a non-empty string')
    }
    out.token = optionAuth.apiKey
  }
  if (optionAuth.username != null) {
    if (typeof optionAuth.username !== 'string' || optionAuth.username.length === 0) {
      throw new TypeError('options.auth.username must be a non-empty string')
    }
    out.username = optionAuth.username
  }
  if (optionAuth.password != null) {
    if (typeof optionAuth.password !== 'string' || optionAuth.password.length === 0) {
      throw new TypeError('options.auth.password must be a non-empty string')
    }
    out.password = optionAuth.password
  }
  if (optionAuth.loginUrl != null) {
    out.loginUrl = optionAuth.loginUrl
  }
  return out
}
