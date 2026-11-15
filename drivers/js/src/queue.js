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

  // Spec §6.1 `queues.create`: idempotent (CREATE QUEUE IF NOT EXISTS) so
  // conformance fixtures can prime a queue the same way the Rust/Go harnesses do.
  create(queue) {
    return this.client.call('query', {
      sql: `CREATE QUEUE IF NOT EXISTS ${queueIdentifier(queue)}`,
    })
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

  // Live `QUEUE READ … WAIT <ms>` helper (PRD #718 / #725). Blocks until
  // a message is available for `consumer` on `queue` (optionally scoped
  // to `group`), the wait budget elapses, or the server cancels.
  //
  // Timeout returns the same empty array as a non-waiting empty pop —
  // never an exception. `waitMs` is required; there is no infinite-wait
  // default. Server-side cancellation, transport cancellation, and cap
  // rejection surface as RedDBErrors from the transport path.
  async readWait(queue, consumer, options = {}) {
    return queuePayloads({ rows: await this.readWaitEntries(queue, consumer, options) })
  }

  async readWaitEntries(queue, consumer, options = {}) {
    const sql = buildQueueReadWaitSql(queue, consumer, options)
    const result = await this.client.call('query', { sql })
    return Array.isArray(result?.rows) ? result.rows : []
  }
}

export class ScopedQueueClient {
  constructor(queueClient, queue) {
    this.queueClient = queueClient
    this.name = queue
  }

  create() { return this.queueClient.create(this.name) }
  push(value, options = {}) { return this.queueClient.push(this.name, value, options) }
  pop(count) { return this.queueClient.pop(this.name, count) }
  peek(count) { return this.queueClient.peek(this.name, count) }
  len() { return this.queueClient.len(this.name) }
  purge() { return this.queueClient.purge(this.name) }
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
  extend(deliveryId) {
    return this.queueClient.client.call('query', {
      sql: `QUEUE EXTEND ${this.name} WITH delivery_id = ${queueStringLiteral(requiredDeliveryId(deliveryId))}`,
    })
  }
  consumer(options) { return new QueueConsumer(this, options) }
}

export class QueueConsumer {
  constructor(queue, options = {}) {
    if (!(queue instanceof ScopedQueueClient)) throw new TypeError('QueueConsumer requires a scoped queue')
    if (typeof options.handleMessage !== 'function') {
      throw new TypeError('queue consumer requires handleMessage(message)')
    }
    this.queue = queue
    this.consumerName = requiredQueueName(options.consumer, 'consumer')
    this.group = options.group == null ? null : requiredQueueName(options.group, 'group')
    this.waitMs = options.waitMs ?? 30_000
    if (!Number.isInteger(this.waitMs) || this.waitMs <= 0) {
      throw new TypeError('queue consumer waitMs must be a positive integer')
    }
    this.handleMessage = options.handleMessage
    this.onError = options.onError ?? null
    if (this.onError != null && typeof this.onError !== 'function') {
      throw new TypeError('queue consumer onError must be a function')
    }
    this.retryDelay = options.retryDelay ?? null
    queueRetryDelay(this.retryDelay)
    this.concurrency = options.concurrency ?? 1
    if (!Number.isInteger(this.concurrency) || this.concurrency < 1 || this.concurrency > 128) {
      throw new TypeError('queue consumer concurrency must be an integer between 1 and 128')
    }
    this.receiveBackoff = receiveBackoffOptions(options.receiveBackoff)
    this.leaseHeartbeatMs = optionalPositiveInteger(
      options.leaseHeartbeatMs,
      'queue consumer leaseHeartbeatMs',
    )
    this.handlerTimeoutMs = optionalPositiveInteger(
      options.handlerTimeoutMs,
      'queue consumer handlerTimeoutMs',
    )
    this.state = 'idle'
    this.paused = false
    this.stopRequested = false
    this.inFlight = 0
    this.loopPromise = null
    this.pauseWaiters = new Set()
    this.sleepWaiters = new Set()
  }

  get status() {
    return {
      state: this.state,
      isRunning: this.state === 'running' || this.state === 'paused',
      isPaused: this.paused,
      inFlight: this.inFlight,
      concurrency: this.concurrency,
    }
  }
  start() {
    if (this.state === 'running' || this.state === 'paused' || this.state === 'stopping') return this
    this.stopRequested = false
    this.paused = false
    this.state = 'running'
    this.loopPromise = this.#run().then(
      () => { this.state = 'stopped' },
      (error) => {
        this.state = 'failed'
        throw error
      },
    )
    return this
  }
  stop() {
    if (this.state === 'idle' || this.state === 'stopped' || this.state === 'failed') {
      return this.finished()
    }
    this.requestStop()
    return this.finished()
  }

  requestStop() {
    if (this.state === 'idle' || this.state === 'stopped' || this.state === 'failed') return this
    this.stopRequested = true
    this.paused = false
    this.state = 'stopping'
    this.#wake(this.pauseWaiters)
    this.#wake(this.sleepWaiters)
    return this
  }
  pause() {
    if (this.state !== 'running') return this
    this.paused = true
    this.state = 'paused'
    return this
  }
  resume() {
    if (this.state !== 'paused') return this
    this.paused = false
    this.state = 'running'
    this.#wake(this.pauseWaiters)
    return this
  }
  finished() { return this.loopPromise ?? Promise.resolve() }

  async #run() {
    let firstError = null
    const lanes = Array.from({ length: this.concurrency }, () => this.#runLane().catch((error) => {
      if (firstError == null) firstError = error
      this.stopRequested = true
      this.paused = false
      this.#wake(this.pauseWaiters)
      this.#wake(this.sleepWaiters)
    }))
    await Promise.all(lanes)
    if (firstError != null) throw firstError
  }

  async #runLane() {
    let receiveFailures = 0
    while (!this.stopRequested) {
      await this.#waitWhilePaused()
      if (this.stopRequested) return
      let entries
      try {
        entries = await this.queue.readWaitEntries(this.consumerName, {
          group: this.group, count: 1, waitMs: this.waitMs,
        })
      } catch (error) {
        if (this.stopRequested) return
        await this.#reportError(error, null)
        receiveFailures += 1
        await this.#sleep(receiveBackoffDelay(this.receiveBackoff, receiveFailures))
        continue
      }
      receiveFailures = 0
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
      await this.#processMessage(message, String(deliveryId))
    }
  }

  async #processMessage(message, deliveryId) {
    this.inFlight += 1
    const controller = new AbortController()
    let handlerError = null
    let timeoutError = null
    let leaseError = null
    let handlerSettled = false
    let finishHeartbeat
    const heartbeatFinished = new Promise((resolve) => { finishHeartbeat = resolve })
    let timeout = null
    if (this.handlerTimeoutMs != null) {
      timeout = setTimeout(() => {
        timeoutError = new RedDBError(
          'QUEUE_HANDLER_TIMEOUT',
          `queue handler exceeded ${this.handlerTimeoutMs}ms`,
        )
        controller.abort(timeoutError)
      }, this.handlerTimeoutMs)
    }
    const heartbeat = this.#heartbeat(
      deliveryId,
      controller,
      heartbeatFinished,
      () => handlerSettled,
      (error) => { leaseError = error },
    )
    try {
      await this.handleMessage(message, {
        signal: controller.signal,
        deliveryId,
        extendLease: () => this.queue.extend(deliveryId),
      })
    } catch (error) {
      handlerError = error
    } finally {
      handlerSettled = true
      finishHeartbeat()
      if (timeout != null) clearTimeout(timeout)
      await heartbeat
      this.inFlight -= 1
    }

    if (leaseError != null) {
      await this.#reportError(leaseError, message)
      return
    }
    const processingError = timeoutError ?? handlerError
    if (processingError != null) {
      try {
        await this.queue.nack(deliveryId, { delay: this.retryDelay })
      } catch (nackError) {
        if (nackError !== processingError && typeof processingError === 'object') {
          processingError.nackError = nackError
        }
      }
      await this.#reportError(processingError, message)
      return
    }
    try {
      await this.queue.ack(deliveryId)
    } catch (error) {
      await this.#reportError(error, message)
    }
  }

  async #heartbeat(deliveryId, controller, finished, isSettled, setLeaseError) {
    if (this.leaseHeartbeatMs == null) return
    while (!isSettled()) {
      await intervalOrFinished(this.leaseHeartbeatMs, finished)
      if (isSettled()) return
      try {
        await this.queue.extend(deliveryId)
      } catch (error) {
        setLeaseError(error)
        controller.abort(error)
        return
      }
    }
  }

  async #waitWhilePaused() {
    while (this.paused && !this.stopRequested) {
      await new Promise((resolve) => { this.pauseWaiters.add(resolve) })
    }
  }

  #sleep(milliseconds) {
    if (milliseconds === 0 || this.stopRequested) return Promise.resolve()
    return new Promise((resolve) => {
      let timer
      const done = () => {
        clearTimeout(timer)
        this.sleepWaiters.delete(done)
        resolve()
      }
      timer = setTimeout(done, milliseconds)
      this.sleepWaiters.add(done)
    })
  }

  #wake(waiters) {
    for (const resolve of [...waiters]) resolve()
    waiters.clear()
  }

  async #reportError(error, message) {
    if (this.onError != null) return this.onError(error, message)
    throw error
  }
}

function receiveBackoffOptions(value) {
  if (value != null && (typeof value !== 'object' || Array.isArray(value))) {
    throw new TypeError('queue consumer receiveBackoff must be an object')
  }
  const options = value ?? {}
  const result = {
    initialMs: options.initialMs ?? 250,
    maxMs: options.maxMs ?? 30_000,
    multiplier: options.multiplier ?? 2,
    jitter: options.jitter ?? true,
  }
  if (!Number.isInteger(result.initialMs) || result.initialMs <= 0) {
    throw new TypeError('queue consumer receiveBackoff.initialMs must be a positive integer')
  }
  if (!Number.isInteger(result.maxMs) || result.maxMs < result.initialMs) {
    throw new TypeError('queue consumer receiveBackoff.maxMs must be an integer >= initialMs')
  }
  if (!Number.isFinite(result.multiplier) || result.multiplier < 1) {
    throw new TypeError('queue consumer receiveBackoff.multiplier must be >= 1')
  }
  if (typeof result.jitter !== 'boolean') {
    throw new TypeError('queue consumer receiveBackoff.jitter must be a boolean')
  }
  return result
}

function receiveBackoffDelay(options, failures) {
  const unjittered = Math.min(
    options.maxMs,
    options.initialMs * (options.multiplier ** Math.max(0, failures - 1)),
  )
  return options.jitter ? Math.floor(Math.random() * (unjittered + 1)) : unjittered
}

function optionalPositiveInteger(value, name) {
  if (value == null) return null
  if (!Number.isInteger(value) || value <= 0) {
    throw new TypeError(`${name} must be a positive integer`)
  }
  return value
}

function intervalOrFinished(milliseconds, finished) {
  return new Promise((resolve) => {
    const timer = setTimeout(resolve, milliseconds)
    finished.then(() => {
      clearTimeout(timer)
      resolve()
    })
  })
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

function requiredQueueName(value, kind = 'queue') {
  if (typeof value !== 'string' || value.length === 0) {
    throw new RedDBError('INVALID_QUEUE_NAME', `${kind} scope requires a non-empty ${kind} name`)
  }
  return queueIdentifier(value)
}

function requiredDeliveryId(value) {
  const deliveryId = String(value ?? '')
  if (deliveryId.length === 0) throw new RedDBError('INVALID_ARGUMENT', 'queue delivery_id must be non-empty')
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
  return Array.isArray(result?.rows) ? result.rows.map((row) => row.payload) : []
}
