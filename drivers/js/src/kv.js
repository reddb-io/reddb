import { RedDBError } from './protocol.js'

export class KvClient {
  constructor(client, collection = 'kv_default') {
    this.client = client
    this.collection = collection
  }

  put(key, value, options = {}) {
    const collection = options.collection ?? this.collection
    const tags = Array.isArray(options.tags) && options.tags.length > 0
      ? ` TAGS [${options.tags.map(kvTagLiteral).join(', ')}]`
      : ''
    const expire = options.expireMs != null ? ` EXPIRE ${Number(options.expireMs)} ms` : ''
    return this.client.call('query', {
      sql: `KV PUT ${kvPath(collection, key)} = ${kvValueLiteral(value)}${expire}${tags}`,
    })
  }

  // Spec-canonical alias for `put` (SDK Helper Spec §5.1 `kv.set`).
  set(key, value, options = {}) {
    return this.put(key, value, options)
  }

  async get(key, options = {}) {
    const collection = options.collection ?? this.collection
    const result = await this.client.call('query', {
      sql: `KV GET ${kvPath(collection, key)}`,
    })
    return result?.rows?.[0]?.value ?? null
  }

  async getMany(keys, options = {}) {
    const values = []
    for (const key of keys) values.push(await this.get(key, options))
    return values
  }

  async exists(key, options = {}) {
    return { exists: (await this.get(key, options)) !== null }
  }

  async delete(key, options = {}) {
    const collection = options.collection ?? this.collection
    const result = await this.client.call('query', {
      sql: `KV DELETE ${kvPath(collection, key)}`,
    })
    const affected = result.affected ?? result.affected_rows ?? 0
    // Spec §5.4 / §2.4 DeleteResult: `deleted` is `affected > 0`.
    return { affected, deleted: affected > 0 }
  }

  async list(options = {}) {
    const collection = options.collection ?? this.collection
    const limit = options.limit == null ? 100 : Number(options.limit)
    if (!Number.isInteger(limit) || limit <= 0) {
      throw new RedDBError('INVALID_ARGUMENT', 'kv.list limit must be a positive integer')
    }
    const prefix = options.prefix == null ? '' : String(options.prefix)
    const result = await this.client.call('query', {
      sql: `SELECT key, value FROM ${kvIdentifier(collection)} ORDER BY key ASC LIMIT ${limit}`,
    })
    const rows = result.rows ?? []
    const items = prefix.length > 0
      ? rows.filter((row) => String(row.key).startsWith(prefix))
      : rows
    return { items }
  }

  async invalidateTags(tags, options = {}) {
    const collection = options.collection ?? this.collection
    const result = await this.client.call('query', {
      sql: `INVALIDATE TAGS [${tags.map(kvTagLiteral).join(', ')}] FROM ${kvIdentifier(collection)}`,
    })
    return result.affected ?? result.affected_rows ?? result.rows?.[0]?.invalidated ?? 0
  }

  async *watch(key, options = {}) {
    if (!this.client.baseUrl) {
      throw new RedDBError('UNSUPPORTED_TRANSPORT', 'kv.watch requires the HTTP transport')
    }
    const collection = options.collection ?? this.collection
    const params = new URLSearchParams()
    if (options.sinceLsn != null) params.set('since_lsn', String(options.sinceLsn))
    if (options.limit != null) params.set('limit', String(options.limit))
    const suffix = params.toString() ? `?${params}` : ''
    const url = `${this.client.baseUrl}/collections/${encodeURIComponent(collection)}/kv/${encodeURIComponent(String(key))}/watch${suffix}`
    const response = await fetch(url, this.client.attachAuth({ method: 'GET' }))
    if (!response.ok) {
      throw new RedDBError('HTTP_ERROR', `kv.watch failed with HTTP ${response.status}`)
    }
    const text = await response.text()
    for (const block of text.split('\n\n')) {
      const line = block.split('\n').find((entry) => entry.startsWith('data: '))
      if (line) yield JSON.parse(line.slice(6))
    }
  }

  watchPrefix(prefix, options = {}) {
    return this.watch(`${prefix}.*`, options)
  }
}

function kvPath(collection, key) {
  return `${kvIdentifier(collection)}.${kvKeySegment(key)}`
}

function kvIdentifier(value) {
  const ident = String(value)
  const invalid = ident.match(/[^A-Za-z0-9_]/)
  if (invalid) {
    throw new RedDBError(
      'INVALID_KV_KEY',
      `invalid KV key "${ident}": character "${invalid[0]}" is not supported`,
    )
  }
  return ident
}

function kvKeySegment(value) {
  const key = String(value)
  if (/^[A-Za-z0-9_]+$/.test(key)) return key
  return `'${key.replace(/'/g, "''")}'`
}

function kvValueLiteral(value) {
  if (typeof value === 'number' || typeof value === 'boolean') return String(value)
  if (value == null) return 'NULL'
  if (typeof value === 'object') return `'${JSON.stringify(value).replace(/'/g, "''")}'`
  return `'${String(value).replace(/'/g, "''")}'`
}

function kvTagLiteral(value) {
  return `'${String(value).replace(/'/g, "''")}'`
}
