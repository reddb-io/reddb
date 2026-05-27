import { RedDBError } from './protocol.js'

export class QueueClient {
  constructor(client) {
    this.client = client
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

  // Live `QUEUE READ … WAIT <ms>` helper (PRD #718 / #725). Same
  // contract as drivers/js: required `waitMs`, timeout returns empty
  // array, cancellation/cap rejection surface as transport errors.
  async readWait(queue, consumer, options = {}) {
    const sql = buildQueueReadWaitSql(queue, consumer, options)
    const result = await this.client.call('query', { sql })
    return queuePayloads(result)
  }
}

function buildQueueReadWaitSql(queue, consumer, options) {
  const { waitMs, group = null, count = null } = options ?? {}
  if (!Number.isInteger(waitMs) || waitMs < 0) {
    throw new RedDBError(
      'INVALID_QUEUE_WAIT',
      'queue readWait requires an explicit non-negative integer waitMs (no infinite wait)',
    )
  }
  const q = queueIdentifier(queue)
  const c = queueIdentifier(consumer)
  const g = group != null ? ` GROUP ${queueIdentifier(group)}` : ''
  const n = count != null ? queueCount(count) : ''
  return `QUEUE READ ${q}${g} CONSUMER ${c}${n} WAIT ${waitMs}ms`
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
