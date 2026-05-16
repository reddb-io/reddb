import { RedDBError } from './protocol.js'

export class DocumentClient {
  constructor(db) {
    this.db = db
  }

  async insert(collection, document) {
    validateObject(document, 'documents.insert document')
    await this.ensureCollection(collection)
    const result = await this.db.query(
      `INSERT INTO ${sqlIdentifierPath(collection)} DOCUMENT (body) VALUES (${sqlJsonLiteral(document)}) RETURNING *`,
    )
    const item = result.rows?.[0]
    if (!item || item.rid == null) {
      throw new RedDBError('INVALID_RESPONSE', 'documents.insert expected one returned item with rid')
    }
    return { affected: result.affected ?? 1, rid: item.rid, item }
  }

  async get(collection, rid) {
    const result = await this.db.get(collection, rid)
    if (!result.entity) {
      throw new RedDBError('NOT_FOUND', `document ${String(rid)} was not found`)
    }
    return result.entity
  }

  async list(collection, options = {}) {
    const limit = normalizeLimit(options.limit)
    const orderBy = options.orderBy ?? options.order_by ?? 'rid ASC'
    const where = options.filter ? ` WHERE ${String(options.filter)}` : ''
    const result = await this.db.query(
      `SELECT * FROM ${sqlIdentifierPath(collection)}${where} ORDER BY ${orderBy} LIMIT ${limit}`,
    )
    return { items: result.rows ?? [] }
  }

  async patch(collection, rid, patch) {
    validateObject(patch, 'documents.patch patch')
    const entries = Object.entries(patch)
    if (entries.length === 0) {
      return this.get(collection, rid)
    }
    for (const [field] of entries) {
      if (field.includes('/')) {
        throw new RedDBError(
          'INVALID_ARGUMENT',
          'documents.patch currently accepts top-level document fields',
        )
      }
    }
    const assignments = entries
      .map(([field, value]) => `${sqlIdentifier(field)} = ${sqlValueLiteral(value)}`)
      .join(', ')
    const result = await this.db.query(
      `UPDATE ${sqlIdentifierPath(collection)} DOCUMENTS SET ${assignments} WHERE rid = $1 RETURNING *`,
      rid,
    )
    const item = result.rows?.[0]
    if (!item) {
      throw new RedDBError('NOT_FOUND', `document ${String(rid)} was not found`)
    }
    return item
  }

  async delete(collection, rid) {
    const result = await this.db.delete(collection, rid)
    return { affected: result.affected ?? 0 }
  }

  async ensureCollection(collection) {
    try {
      await this.db.query(`CREATE DOCUMENT ${sqlIdentifierPath(collection)}`)
    } catch (err) {
      const message = String(err?.message ?? '')
      if (!message.includes('already exists')) throw err
    }
  }
}

function validateObject(value, label) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new RedDBError('INVALID_ARGUMENT', `${label} must be an object`)
  }
}

function normalizeLimit(value) {
  if (value == null) return 100
  if (!Number.isInteger(value) || value <= 0) {
    throw new RedDBError('INVALID_ARGUMENT', 'limit must be a positive integer')
  }
  return value
}

function sqlIdentifierPath(value) {
  return String(value).split('.').map(sqlIdentifier).join('.')
}

function sqlIdentifier(value) {
  const ident = String(value)
  if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(ident)) {
    throw new RedDBError('INVALID_ARGUMENT', `invalid SQL identifier "${ident}"`)
  }
  return ident
}

function sqlJsonLiteral(value) {
  return sqlString(JSON.stringify(value))
}

function sqlValueLiteral(value) {
  if (value == null) return 'NULL'
  if (typeof value === 'number' || typeof value === 'boolean') return String(value)
  if (typeof value === 'object') return sqlJsonLiteral(value)
  return sqlString(value)
}

function sqlString(value) {
  return `'${String(value).replace(/'/g, "''")}'`
}
