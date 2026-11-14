import { RedDBError } from './protocol.js'

export class QueueClient {
  static [Symbol.hasInstance](value) {
    const candidate = value?.__reddbClientInstance ?? value
    return this.prototype.isPrototypeOf(candidate)
  }

  constructor(client) {
    this.client = client
  }

  scope(queue) {
    return new ScopedQueueClient(this, requiredQueueName(queue))
  }

  push(queue, value, options = {}) {
    const suffix = queuePushOptions(options)
    return this.client.call('query', {
      sql: `QUEUE PUSH ${queueIdentifier(queue)} ${queueValueLiteral(value)}${suffix}`,
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

  async readWait(queue, consumer, options = {}) {
    return queuePayloads({ rows: await this.readWaitEntries(queue, consumer, options) })
  }

  // RedWire's QueueWaitOpen parks on the async wait registry instead of
  // occupying an engine execution slot for the wait duration. The v1 frame
  // has no terminal marker after a multi-message delivery, so COUNT != 1
  // deliberately stays on the SQL compatibility path.
  async readWaitEntries(queue, consumer, options = {}) {
    const request = buildQueueReadWaitRequest(queue, consumer, options)
    if (this.client.nativeQueueWait === true && request.count === 1) {
      const { sql: _, ...params } = request
      const result = await this.client.call('queue_wait', params)
      return queueRows(result)
    }
    const result = await this.client.call('query', { sql: request.sql })
    return queueRows(result)
  }
}

export class ScopedQueueClient {
  constructor(queueClient, queue) {
    this.queueClient = queueClient
    this.name = queue
  }

  push(value, options = {}) {
    return this.queueClient.push(this.name, value, options)
  }

  pop(count) {
    return this.queueClient.pop(this.name, count)
  }

  peek(count) {
    return this.queueClient.peek(this.name, count)
  }

  len() {
    return this.queueClient.len(this.name)
  }

  purge() {
    return this.queueClient.purge(this.name)
  }

  readWait(consumer, options = {}) {
    return this.queueClient.readWait(this.name, consumer, options)
  }

  readWaitEntries(consumer, options = {}) {
    return this.queueClient.readWaitEntries(this.name, consumer, options)
  }

  ack(deliveryId) {
    return this.queueClient.client.call('query', {
      sql: `QUEUE ACK ${this.name} WITH delivery_id = ${queueStringLiteral(requiredDeliveryId(deliveryId))}`,
    })
  }

  nack(deliveryId, options = {}) {
    const delay = queueRetryDelay(options.delay)
    return this.queueClient.client.call('query', {
      sql: `QUEUE NACK ${this.name} WITH delivery_id = ${queueStringLiteral(requiredDeliveryId(deliveryId))}${delay}`,
    })
  }

  consumer(options) {
    return new QueueConsumer(this, options)
  }
}

export class QueueConsumer {
  constructor(queue, options = {}) {
    if (!(queue instanceof ScopedQueueClient)) {
      throw new TypeError('QueueConsumer requires a scoped queue')
    }
    if (typeof options.handleMessage !== 'function') {
      throw new TypeError('queue consumer requires handleMessage(message)')
    }
    this.queue = queue
    this.consumerName = requiredQueueName(options.consumer, 'consumer')
    this.group = options.group == null ? null : requiredQueueName(options.group, 'group')
    this.waitMs = options.waitMs ?? 30_000
    if (!Number.isInteger(this.waitMs) || this.waitMs < 0) {
      throw new TypeError('queue consumer waitMs must be a non-negative integer')
    }
    this.handleMessage = options.handleMessage
    this.onError = options.onError ?? null
    if (this.onError != null && typeof this.onError !== 'function') {
      throw new TypeError('queue consumer onError must be a function')
    }
    this.retryDelay = options.retryDelay ?? null
    queueRetryDelay(this.retryDelay)
    this.running = false
    this.loopPromise = null
  }

  get status() {
    return { isRunning: this.running }
  }

  start() {
    if (this.running) return this
    this.running = true
    this.loopPromise = this.#run().finally(() => {
      this.running = false
    })
    return this
  }

  stop() {
    this.running = false
    return this.finished()
  }

  finished() {
    return this.loopPromise ?? Promise.resolve()
  }

  async #run() {
    while (this.running) {
      let entries
      try {
        entries = await this.queue.readWaitEntries(this.consumerName, {
          group: this.group,
          count: 1,
          waitMs: this.waitMs,
        })
      } catch (error) {
        if (!this.running) return
        await this.#reportError(error, null)
        continue
      }
      if (entries.length === 0) continue
      const message = entries[0]
      const deliveryId = message?.delivery_id
      if (deliveryId == null || String(deliveryId).length === 0) {
        await this.#reportError(
          new RedDBError('INVALID_QUEUE_DELIVERY', 'queue delivery is missing delivery_id'),
          message,
        )
        continue
      }
      try {
        await this.handleMessage(message)
      } catch (error) {
        try {
          await this.queue.nack(deliveryId, { delay: this.retryDelay })
        } catch (nackError) {
          if (nackError !== error && error != null && typeof error === 'object') {
            error.nackError = nackError
          }
        }
        await this.#reportError(error, message)
        continue
      }
      try {
        await this.queue.ack(deliveryId)
      } catch (error) {
        await this.#reportError(error, message)
      }
    }
  }

  async #reportError(error, message) {
    if (this.onError != null) {
      await this.onError(error, message)
      return
    }
    throw error
  }
}

function buildQueueReadWaitRequest(queue, consumer, options) {
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
  return {
    queue: q,
    group: group != null ? queueIdentifier(group) : null,
    consumer: c,
    count: count ?? 1,
    waitMs,
    sql: `QUEUE READ ${q}${g} CONSUMER ${c}${n} WAIT ${waitMs}ms`,
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

function requiredQueueName(value, kind = 'queue') {
  if (typeof value !== 'string' || value.length === 0) {
    throw new RedDBError('INVALID_QUEUE_NAME', `${kind} scope requires a non-empty ${kind} name`)
  }
  return queueIdentifier(value)
}

function requiredDeliveryId(value) {
  const deliveryId = String(value ?? '')
  if (deliveryId.length === 0) {
    throw new RedDBError('INVALID_ARGUMENT', 'queue delivery_id must be non-empty')
  }
  return deliveryId
}

function queueRetryDelay(value) {
  if (value == null) return ''
  const delay = String(value)
  if (!/^(?:\d+(?:\.\d+)?)(?:ms|s|sec|secs|m|min|mins|h|hr|hrs|d|day|days)?$/.test(delay)) {
    throw new RedDBError('INVALID_ARGUMENT', `invalid queue retry delay "${delay}"`)
  }
  return ` WITH DELAY ${delay}`
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

function queuePushOptions(options) {
  if (options.key != null && (options.delay != null || options.at != null)) {
    throw new RedDBError(
      'INVALID_ARGUMENT',
      'QUEUE PUSH KEY cannot be combined with DELAY / AVAILABLE AT',
    )
  }
  let suffix = ''
  if (options.priority != null) suffix += ` PRIORITY ${queuePriority(options.priority)}`
  if (options.key != null) suffix += ` KEY ${queueStringLiteral(options.key)}`
  if (options.dedup != null) suffix += ` DEDUP ${queueStringLiteral(options.dedup)}`
  if (options.delay != null) suffix += ` DELAY ${options.delay}`
  if (options.at != null) suffix += ` AVAILABLE AT ${options.at}`
  return suffix
}

function queueValueLiteral(value) {
  if (typeof value === 'number' || typeof value === 'boolean') return String(value)
  if (value == null) return 'NULL'
  if (typeof value === 'string') return queueStringLiteral(value)
  return JSON.stringify(value)
}

function queueStringLiteral(value) {
  return `'${String(value).replace(/\\/g, '\\\\').replace(/'/g, "''")}'`
}

function queuePayloads(result) {
  return queueRows(result).map((row) => row.payload)
}

function queueRows(result) {
  return Array.isArray(result?.rows) ? result.rows : []
}
