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
}

function kvPath(collection, key) {
  return `${kvIdentifier(collection)}.${kvIdentifier(key)}`
}

function kvIdentifier(value) {
  return String(value).replace(/[^A-Za-z0-9_]/g, '_')
}

function kvValueLiteral(value) {
  if (typeof value === 'number' || typeof value === 'boolean') return String(value)
  if (value == null) return 'NULL'
  return `'${String(value).replace(/'/g, "''")}'`
}

function kvTagLiteral(value) {
  return `'${String(value).replace(/'/g, "''")}'`
}
