export class ConfigClient {
  constructor(client, collection = 'red.config') {
    this.client = client
    this.collection = collection
  }

  put(key, value, options = {}) {
    rejectVolatileOptions(options, 'config')
    const collection = options.collection ?? this.collection
    const tags = Array.isArray(options.tags) && options.tags.length > 0
      ? ` TAGS [${options.tags.map(keyedStringLiteral).join(', ')}]`
      : ''
    return this.client.call('query', {
      sql: `PUT CONFIG ${keyedIdentifier(collection)} ${keyedIdentifier(key)} = ${configValueLiteral(value, options)}${tags}`,
    })
  }

  get(key, options = {}) {
    const collection = options.collection ?? this.collection
    return this.client.call('query', {
      sql: `GET CONFIG ${keyedIdentifier(collection)} ${keyedIdentifier(key)}`,
    })
  }

  resolve(key, options = {}) {
    const collection = options.collection ?? this.collection
    return this.client.call('query', {
      sql: `RESOLVE CONFIG ${keyedIdentifier(collection)} ${keyedIdentifier(key)}`,
    })
  }
}

function configValueLiteral(value, options) {
  if (options.secretRef) {
    const { collection, key } = options.secretRef
    return `SECRET_REF(vault, ${keyedIdentifier(collection)}.${keyedIdentifier(key)})`
  }
  return keyedValueLiteral(value)
}

function rejectVolatileOptions(options, domain) {
  for (const field of ['ttl', 'ttlMs', 'ttl_ms', 'expireMs', 'expire_ms', 'expiresAt']) {
    if (options[field] != null) {
      throw new TypeError(`${domain} does not support TTL or expiration options`)
    }
  }
}

function keyedIdentifier(value) {
  const out = String(value)
  if (!/^[A-Za-z0-9_.]+$/.test(out)) {
    throw new TypeError('keyed collection and key names must use letters, numbers, underscores, or dots')
  }
  return out
}

function keyedValueLiteral(value) {
  if (typeof value === 'number' || typeof value === 'boolean') return String(value)
  if (value == null) return 'NULL'
  if (Array.isArray(value) || typeof value === 'object') return JSON.stringify(value)
  return keyedStringLiteral(value)
}

function keyedStringLiteral(value) {
  return `'${String(value).replace(/'/g, "''")}'`
}
