/**
 * Canonical query result.
 *
 * Every transport must hand callers the same shape,
 * `{ statement, affected, columns, rows }`, with `rows` an array of plain
 * objects keyed by column name. The wire does not: the HTTP and RedWire
 * (`QueryWithParams`) replies are an envelope
 * `{ ok, statement, affected_rows, result: { columns, records: [{ values, meta }], stats } }`,
 * and the gRPC reply carries `records` as well. This turns any of them into
 * the canonical shape; a value that is already canonical is returned as is,
 * so the function is safe to apply more than once.
 *
 * Pure: no `node:` imports, no I/O.
 */

function isObject(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}

function firstNumber(...candidates) {
  for (const candidate of candidates) {
    if (typeof candidate === 'number' || typeof candidate === 'bigint') return candidate
  }
  return undefined
}

/** A record is `{ values, meta, ... }`; the row a caller wants is `values`. */
function recordToRow(record) {
  return isObject(record) && isObject(record.values) ? record.values : record
}

function columnNames(raw, body) {
  if (Array.isArray(body.columns)) return body.columns
  if (Array.isArray(raw.columns)) return raw.columns
  const described = raw.descriptor?.columns
  if (Array.isArray(described)) {
    return described.map((column) => (isObject(column) ? column.name : column))
  }
  return []
}

/**
 * @param {unknown} raw a reply from any transport
 * @returns {{ statement: string, affected: number | bigint, columns: string[], rows: object[] }}
 *   plus `ok`, `bookmark` and `stats` when the server supplied them
 */
export function normalizeQueryResult(raw) {
  if (!isObject(raw)) return raw
  const body = isObject(raw.result) ? raw.result : raw
  const records = Array.isArray(body.records) ? body.records : null
  const hasRows = Array.isArray(body.rows) || Array.isArray(raw.rows)
  if (!records && !hasRows && !('affected_rows' in raw) && !('affected' in raw) && !isObject(raw.result)) {
    // Not a query reply at all (e.g. a bare health/version object): leave it alone.
    return raw
  }

  const rows = Array.isArray(body.rows)
    ? body.rows
    : Array.isArray(raw.rows)
      ? raw.rows
      : records
        ? records.map(recordToRow)
        : []

  const result = {
    statement: raw.statement_type ?? raw.statement ?? body.statement ?? '',
    affected: firstNumber(raw.affected, raw.affected_rows, body.affected, body.affected_rows) ?? 0,
    columns: columnNames(raw, body),
    rows,
  }
  if (raw.ok !== undefined) result.ok = raw.ok
  if (raw.bookmark !== undefined) result.bookmark = raw.bookmark
  const stats = body.stats ?? raw.stats
  if (stats !== undefined) result.stats = stats
  return result
}
