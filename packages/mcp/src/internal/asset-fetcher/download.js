import { request as httpsRequest } from 'node:https'
import { request as httpRequest } from 'node:http'

const MAX_REDIRECTS = 5

/**
 * Hosts a release download may be served from. The postinstall script writes
 * the response to disk and then executes it, so an unrestricted redirect
 * chain (or a plain-http hop) turns any network position into code
 * execution on the installing machine. GitHub serves release assets from
 * `github.com` and redirects to its object storage.
 */
const ALLOWED_HOSTS = new Set([
  'github.com',
  'objects.githubusercontent.com',
  'release-assets.githubusercontent.com',
  'raw.githubusercontent.com',
])

/**
 * Ceiling on a downloaded asset. The `red` binary is tens of megabytes; a
 * response without this cap is buffered in full whatever its size.
 */
const MAX_BODY_BYTES = 512 * 1024 * 1024

export class DisallowedHostError extends Error {
  constructor(url) {
    super(`refusing to download from a non-allowlisted host: ${url}`)
    this.name = 'DisallowedHostError'
    this.code = 'DISALLOWED_HOST'
    this.url = url
  }
}

export class ResponseTooLargeError extends Error {
  constructor(url, limit) {
    super(`response from ${url} exceeds the ${limit}-byte limit`)
    this.name = 'ResponseTooLargeError'
    this.code = 'RESPONSE_TOO_LARGE'
    this.url = url
    this.limit = limit
  }
}

/**
 * Reject anything that is not https on an allowlisted host. Applied to the
 * initial URL and to every redirect target, since a redirect is exactly how
 * an attacker would leave the allowlist.
 *
 * `allowedHosts` exists so the test suite can point the transport at a local
 * server; production callers never pass it and get {@link ALLOWED_HOSTS}
 * plus the https requirement.
 */
export function assertDownloadableUrl(url, allowedHosts = ALLOWED_HOSTS) {
  let parsed
  try {
    parsed = new URL(url)
  } catch {
    throw new DisallowedHostError(url)
  }
  const isDefaultPolicy = allowedHosts === ALLOWED_HOSTS
  if (isDefaultPolicy && parsed.protocol !== 'https:') {
    throw new DisallowedHostError(url)
  }
  if (!allowedHosts.has(parsed.hostname)) {
    throw new DisallowedHostError(url)
  }
  return parsed
}

export class AssetNotFoundError extends Error {
  constructor(url) {
    super(`asset not found (HTTP 404) at ${url}`)
    this.name = 'AssetNotFoundError'
    this.code = 'ASSET_NOT_FOUND'
    this.url = url
  }
}

export class HttpError extends Error {
  constructor(status, url) {
    super(`HTTP ${status} fetching ${url}`)
    this.name = 'HttpError'
    this.code = 'HTTP_ERROR'
    this.status = status
    this.url = url
  }
}

export class TooManyRedirectsError extends Error {
  constructor(url) {
    super(`too many redirects (>${MAX_REDIRECTS}) starting at ${url}`)
    this.name = 'TooManyRedirectsError'
    this.code = 'TOO_MANY_REDIRECTS'
    this.url = url
  }
}

function resolveLocation(currentUrl, location) {
  if (/^https?:\/\//i.test(location)) return location
  return new URL(location, currentUrl).toString()
}

export function downloadFollowingRedirects(
  url,
  { userAgent, originalUrl, allowedHosts } = {},
  depth = 0,
) {
  const startUrl = originalUrl || url
  if (depth > MAX_REDIRECTS) {
    return Promise.reject(new TooManyRedirectsError(startUrl))
  }
  let parsed
  try {
    parsed = assertDownloadableUrl(url, allowedHosts)
  } catch (err) {
    return Promise.reject(err)
  }
  const request = parsed.protocol === 'http:' ? httpRequest : httpsRequest
  return new Promise((resolve, reject) => {
    const req = request(
      url,
      {
        method: 'GET',
        headers: {
          'User-Agent': userAgent || 'reddb-internal-asset-fetcher',
          Accept: 'application/octet-stream',
        },
      },
      (res) => {
        const status = res.statusCode || 0
        if (status >= 300 && status < 400 && res.headers.location) {
          res.resume()
          const next = resolveLocation(url, res.headers.location)
          downloadFollowingRedirects(
            next,
            { userAgent, originalUrl: startUrl, allowedHosts },
            depth + 1,
          ).then(resolve, reject)
          return
        }
        if (status === 404) {
          res.resume()
          reject(new AssetNotFoundError(startUrl))
          return
        }
        if (status < 200 || status >= 300) {
          res.resume()
          reject(new HttpError(status, url))
          return
        }
        const chunks = []
        let received = 0
        res.on('data', (chunk) => {
          received += chunk.length
          if (received > MAX_BODY_BYTES) {
            res.destroy()
            reject(new ResponseTooLargeError(startUrl, MAX_BODY_BYTES))
            return
          }
          chunks.push(chunk)
        })
        res.on('end', () => resolve(Buffer.concat(chunks)))
        res.on('error', reject)
      },
    )
    req.on('error', reject)
    req.end()
  })
}
