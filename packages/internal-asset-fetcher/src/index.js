/**
 * @reddb-io/internal-asset-fetcher — fetch a `red`/`red_client` binary
 * from a GitHub release.
 *
 * Public surface: one function.
 *
 *   fetchReleaseAsset({ repo, tag, platform, arch, binName, sha256? }) → Buffer
 *
 * Steps:
 *   1. Map (platform, arch, binName) → asset filename.
 *   2. Compose the GitHub download URL: `https://github.com/<repo>/releases/download/<tag>/<asset>`.
 *   3. Follow up to 5 redirects, returning the final body as a Buffer.
 *   4. If `sha256` was supplied, verify before returning.
 *
 * Errors carry distinct `.code` values so callers can differentiate:
 *   - UNSUPPORTED_PLATFORM   — no asset for this platform/arch
 *   - ASSET_NOT_FOUND        — HTTP 404 (release/tag/asset name wrong)
 *   - CHECKSUM_MISMATCH      — body downloaded but sha256 mismatched
 *   - HTTP_ERROR             — any other non-2xx status
 *   - TOO_MANY_REDIRECTS     — redirect chain longer than 5 hops
 *
 * Internal modules (`./asset-name.js`, `./download.js`, `./checksum.js`)
 * are not part of the public contract — only `fetchReleaseAsset` is.
 * They are imported directly in tests for focused coverage.
 */

import { composeAssetName } from './asset-name.js'
import { downloadFollowingRedirects } from './download.js'
import { sha256Hex, verifySha256, ChecksumMismatchError } from './checksum.js'

/**
 * A GitHub `owner/name` pair. `repo` reaches this function from
 * `REDDB_POSTINSTALL_REPO` / `REDDB_MCP_REPO`, and is interpolated into the
 * download URL, so a value containing a path or authority would point the
 * download somewhere else entirely.
 */
const REPO_PATTERN = /^[A-Za-z0-9._-]+\/[A-Za-z0-9._-]+$/

/** A release tag, as it appears in the download URL path. */
const TAG_PATTERN = /^[A-Za-z0-9._+-]+$/

export class ChecksumUnavailableError extends Error {
  constructor(assetName) {
    super(
      `no SHA256SUMS entry for ${assetName}; refusing to install an unverified binary ` +
        `(set REDDB_POSTINSTALL_ALLOW_UNVERIFIED=1 to override)`,
    )
    this.name = 'ChecksumUnavailableError'
    this.code = 'CHECKSUM_UNAVAILABLE'
    this.assetName = assetName
  }
}

/**
 * Parse a `SHA256SUMS` file (`<hex>  <filename>` per line) and return the
 * digest recorded for `assetName`, or `null` when it is not listed.
 */
export function sha256FromSumsFile(text, assetName) {
  for (const line of String(text).split('\n')) {
    const match = line.trim().match(/^([0-9a-fA-F]{64})\s+\*?(.+)$/)
    if (match && match[2].trim() === assetName) {
      return match[1].toLowerCase()
    }
  }
  return null
}

export async function fetchReleaseAsset({ repo, tag, platform, arch, binName, sha256 } = {}) {
  if (typeof repo !== 'string' || repo === '') {
    throw new TypeError('fetchReleaseAsset: `repo` must be a non-empty string (e.g. "reddb-io/reddb")')
  }
  if (typeof tag !== 'string' || tag === '') {
    throw new TypeError('fetchReleaseAsset: `tag` must be a non-empty string (e.g. "v0.2.9")')
  }
  if (typeof platform !== 'string' || platform === '') {
    throw new TypeError('fetchReleaseAsset: `platform` must be a non-empty string')
  }
  if (typeof arch !== 'string' || arch === '') {
    throw new TypeError('fetchReleaseAsset: `arch` must be a non-empty string')
  }

  if (!REPO_PATTERN.test(repo)) {
    throw new TypeError(`fetchReleaseAsset: \`repo\` must be "owner/name", got ${JSON.stringify(repo)}`)
  }
  if (!TAG_PATTERN.test(tag)) {
    throw new TypeError(`fetchReleaseAsset: \`tag\` contains unsupported characters: ${JSON.stringify(tag)}`)
  }

  const assetName = composeAssetName({ platform, arch, binName })
  const base = `https://github.com/${repo}/releases/download/${tag}`
  const body = await downloadFollowingRedirects(`${base}/${assetName}`)

  // The caller may pin a digest; otherwise fall back to the `SHA256SUMS`
  // file the release publishes beside the assets. The downloaded bytes are
  // written to disk and executed, so installing them unverified is the one
  // outcome worth failing the install over — `install.sh` has verified since
  // it shipped, and this path had not.
  if (sha256) {
    verifySha256(body, sha256)
    return body
  }

  let expected = null
  try {
    const sums = await downloadFollowingRedirects(`${base}/SHA256SUMS`)
    expected = sha256FromSumsFile(sums.toString('utf8'), assetName)
  } catch {
    expected = null
  }
  if (expected) {
    if (sha256Hex(body) !== expected) {
      throw new ChecksumMismatchError(expected, sha256Hex(body))
    }
    return body
  }
  if (allowUnverified()) {
    return body
  }
  throw new ChecksumUnavailableError(assetName)
}

/**
 * Escape hatch for installing from a release that predates `SHA256SUMS`, or
 * from a fork that does not publish one. Opt-in and loud rather than the
 * silent default it used to be.
 */
function allowUnverified() {
  const raw = process.env.REDDB_POSTINSTALL_ALLOW_UNVERIFIED
  return raw === '1' || raw === 'true' || raw === 'yes'
}
