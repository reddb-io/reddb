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
import { verifySha256 } from './checksum.js'

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

  const assetName = composeAssetName({ platform, arch, binName })
  const url = `https://github.com/${repo}/releases/download/${tag}/${assetName}`
  const body = await downloadFollowingRedirects(url)
  if (sha256) {
    verifySha256(body, sha256)
  }
  return body
}
