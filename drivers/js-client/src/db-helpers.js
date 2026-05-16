import { RedDBError } from './protocol.js'

export async function listCollections(db) {
  const result = await db.query('SHOW COLLECTIONS')
  return (result.rows ?? []).map(collectionMeta)
}

export async function collectionExists(db, collection) {
  const result = await db.query(`SHOW COLLECTIONS WHERE name = ${sqlString(collection)}`)
  return (result.rows ?? []).some((row) => row.name === String(collection))
}

export class TypedQueryBuilder {
  constructor(db, collection, columns = null, whereClauses = [], params = []) {
    this.db = db
    this.collection = collection
    this.columns = columns
    this.whereClauses = whereClauses
    this.params = params
  }

  select(...columns) {
    const selected = columns.length === 1 && Array.isArray(columns[0]) ? columns[0] : columns
    const projection = selected.length === 1 && selected[0] === '*' ? null : selected
    return new TypedQueryBuilder(
      this.db,
      this.collection,
      projection != null && projection.length > 0 ? projection : null,
      this.whereClauses,
      this.params,
    )
  }

  where(condition, ...params) {
    if (typeof condition !== 'string' || condition.trim().length === 0) {
      throw new RedDBError('INVALID_QUERY_BUILDER', 'where() requires a non-empty SQL condition')
    }
    const nextParams = params.length === 1 && Array.isArray(params[0]) ? params[0] : params
    return new TypedQueryBuilder(
      this.db,
      this.collection,
      this.columns,
      [...this.whereClauses, condition.trim()],
      [...this.params, ...nextParams],
    )
  }

  async run() {
    const projection = this.columns == null
      ? '*'
      : this.columns.map(sqlIdentifierPath).join(', ')
    const where = this.whereClauses.length > 0
      ? ` WHERE ${this.whereClauses.join(' AND ')}`
      : ''
    const sql = `SELECT ${projection} FROM ${sqlIdentifierPath(this.collection)}${where}`
    const result = this.params.length > 0
      ? await this.db.query(sql, this.params)
      : await this.db.query(sql)
    const rows = result.rows ?? []
    if (this.columns == null) return rows
    return rows.map((row) => {
      const selected = {}
      for (const column of this.columns) selected[column] = row[column]
      return selected
    })
  }
}

function collectionMeta(row) {
  return {
    ...row,
    name: String(row.name),
    model: String(row.model),
    capabilities: Array.isArray(row.capabilities) ? row.capabilities : [],
  }
}

function sqlIdentifierPath(value) {
  return String(value).split('.').map(sqlIdentifier).join('.')
}

function sqlIdentifier(value) {
  const ident = String(value)
  if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(ident)) {
    throw new RedDBError(
      'INVALID_IDENTIFIER',
      `invalid SQL identifier "${ident}"`,
    )
  }
  return ident
}

function sqlString(value) {
  return `'${String(value).replace(/'/g, "''")}'`
}
