import { request as httpsRequest } from 'node:https'
import { request as httpRequest } from 'node:http'

const MAX_REDIRECTS = 5

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

function pickRequest(url) {
  return url.startsWith('http://') ? httpRequest : httpsRequest
}

function resolveLocation(currentUrl, location) {
  if (/^https?:\/\//i.test(location)) return location
  return new URL(location, currentUrl).toString()
}

export function downloadFollowingRedirects(url, { userAgent, originalUrl } = {}, depth = 0) {
  const startUrl = originalUrl || url
  if (depth > MAX_REDIRECTS) {
    return Promise.reject(new TooManyRedirectsError(startUrl))
  }
  const request = pickRequest(url)
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
          downloadFollowingRedirects(next, { userAgent, originalUrl: startUrl }, depth + 1).then(
            resolve,
            reject,
          )
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
        res.on('data', (chunk) => chunks.push(chunk))
        res.on('end', () => resolve(Buffer.concat(chunks)))
        res.on('error', reject)
      },
    )
    req.on('error', reject)
    req.end()
  })
}
