import { RedDBError } from './protocol.js'

export class QueueClient {
  constructor(client) {
    this.client = client
  }

  // Spec §6.1 `queues.create`: idempotent (CREATE QUEUE IF NOT EXISTS) so
  // conformance fixtures can prime a queue the same way the Rust/Go harnesses do.
  create(queue) {
    return this.client.call('query', {
      sql: `CREATE QUEUE IF NOT EXISTS ${queueIdentifier(queue)}`,
    })
  }

  push(queue, value, options = {}) {
    const priority = options.priority != null ? ` PRIORITY ${queuePriority(options.priority)}` : ''
    return this.client.call('query', {
      sql: `QUEUE PUSH ${queueIdentifier(queue)} ${queueValueLiteral(value)}${priority}`,
    })
  }

  async pop(queue, count) {
    const result = await this.client.call('query', {
      sql: `QUEUE POP ${queueIdentifier(queue)}${queueCount(count)}`,
    })
    return queuePayloads(result)
  }

  async peek(queue, count) {
    const result = await this.client.call('query', {
      sql: `QUEUE PEEK ${queueIdentifier(queue)}${queueCount(count)}`,
    })
    return queuePayloads(result)
  }

  async len(queue) {
    const result = await this.client.call('query', {
      sql: `QUEUE LEN ${queueIdentifier(queue)}`,
    })
    return Number(result?.rows?.[0]?.len ?? 0)
  }

  purge(queue) {
    return this.client.call('query', {
      sql: `QUEUE PURGE ${queueIdentifier(queue)}`,
    })
  }
}

function queueIdentifier(value) {
  const ident = String(value)
  if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(ident)) {
    throw new RedDBError(
      'INVALID_QUEUE_NAME',
      `invalid queue name "${ident}": expected an SQL identifier`,
    )
  }
  return ident
}

function queueCount(count) {
  if (count == null) return ''
  if (!Number.isInteger(count) || count < 0) {
    throw new RedDBError('INVALID_QUEUE_COUNT', 'queue count must be a non-negative integer')
  }
  return ` COUNT ${count}`
}

function queuePriority(priority) {
  if (!Number.isInteger(priority)) {
    throw new RedDBError('INVALID_QUEUE_PRIORITY', 'queue priority must be an integer')
  }
  return String(priority)
}

function queueValueLiteral(value) {
  if (typeof value === 'number' || typeof value === 'boolean') return String(value)
  if (value == null) return 'NULL'
  if (typeof value === 'string') return `'${value.replace(/'/g, "''")}'`
  return JSON.stringify(value)
}

function queuePayloads(result) {
  return Array.isArray(result?.rows) ? result.rows.map((row) => row.payload) : []
}
