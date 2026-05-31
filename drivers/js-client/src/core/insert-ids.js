/**
 * Insert-id normalization for `insert()` / `bulkInsert()` results.
 *
 * The server returns either `rid`/`rids` (current) or `id`/`ids` (legacy);
 * these helpers mirror one onto the other so callers see both, and raise
 * `ENGINE_TOO_OLD` when neither is present. Imports zero `node:` built-ins.
 */

import { RedDBError } from './errors.js'

export const MIN_INSERT_ID_ENGINE_VERSION = '1.0.9'

export function requireInsertId(result, method) {
  if (!result || typeof result !== 'object' || (result.rid == null && result.id == null)) {
    throw new RedDBError(
      'ENGINE_TOO_OLD',
      `${method}() requires RedDB engine >= ${MIN_INSERT_ID_ENGINE_VERSION} with insert id support`,
    )
  }
  if (result.rid == null) result.rid = result.id
  if (result.id == null) result.id = result.rid
  return result
}

export function requireInsertIds(result, expected) {
  if (
    !result ||
    typeof result !== 'object' ||
    (!Array.isArray(result.rids) && !Array.isArray(result.ids))
  ) {
    throw new RedDBError(
      'ENGINE_TOO_OLD',
      `bulkInsert() requires RedDB engine >= ${MIN_INSERT_ID_ENGINE_VERSION} with bulk insert id support`,
    )
  }
  if (!Array.isArray(result.rids)) result.rids = result.ids
  if (!Array.isArray(result.ids)) result.ids = result.rids
  if (result.rids.length !== expected) {
    throw new RedDBError(
      'INVALID_RESPONSE',
      `bulkInsert() expected ${expected} rids, got ${result.rids.length}`,
    )
  }
  return result
}
