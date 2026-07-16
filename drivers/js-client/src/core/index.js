/**
 * Transport-agnostic core of `@reddb-io/client`.
 *
 * Everything re-exported here is pure JS with **zero `node:` imports**: the
 * `RedDB` connection handle (and `Collection` / transaction handles), SQL
 * parameter serialization, the `login()` / auth-merge credential flow,
 * insert-id normalization, embedded-URI rejection, the `red://` URL parser,
 * and the NDJSON frame helpers.
 *
 * Streaming is injected, not imported: `RedDB` takes a streaming
 * implementation as a constructor argument (see `core/reddb.js`). The Node
 * entry (`../index.js`) supplies the `node:stream`-based one. A browser
 * entry (separate slice) builds on this same seam.
 */

export { RedDBError } from './errors.js'
export { RedDB, Collection } from './reddb.js'
export {
  serializeParam,
  serializeJsonValue,
  normalizeExactNumbers,
  assertSupportedParam,
  normalizeQueryParams,
  bytesToBase64,
  isUuidString,
} from './serialization.js'
export { login, mergeAuthFromUri } from './auth.js'
export {
  MIN_INSERT_ID_ENGINE_VERSION,
  requireInsertId,
  requireInsertIds,
} from './insert-ids.js'
export { classifyNdjsonFrame, splitLines } from './ndjson.js'
export {
  EMBEDDED_REJECTION_MESSAGE,
  EmbeddedNotSupported,
  isEmbeddedUri,
  rejectEmbeddedUri,
} from './embedded-rejection.js'
export {
  parseUri,
  parseRedUrl,
  parseLegacyUrl,
  deriveLoginUrl,
} from './url.js'
