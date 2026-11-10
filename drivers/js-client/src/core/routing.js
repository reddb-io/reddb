import { RedDBError } from './errors.js'
import { isReplicaQuery, isSessionQuery, topologyAddress, transactionCommand } from './topology.js'
import { parseUri } from './url.js'
import { mergeAuthFromUri } from './auth.js'

export async function connectRouted(uri, options, connectDirect) {
  const db = await connectDirect(uri, options)
  const parsed = parseUri(uri)
  let protocol = { redws: 'ws', redwss: 'wss' }[parsed.kind] ?? parsed.kind
  if (protocol === 'red' && (options.tls || ['true', '1'].includes(parsed.params?.get('tls')))) protocol = 'reds'
  const seed = topologyAddress(`${protocol}://${parsed.host}:${parsed.port}`, protocol)
  const auth = mergeAuthFromUri(parsed, options.auth)
  const dial = async (address, signal) => {
    const target = new URL(address)
    target.search = parsed.params?.toString() ?? ''
    return (await connectDirect(target.href, { ...options, auth, signal }, true)).client
  }
  const router = new RoutingClient(seed, db.client, dial, protocol, {
    forcePrimary: parsed.params?.get('route') === 'primary',
  })
  try {
    db.client = await router.initialize()
    // Helper clients retain the low-level transport supplied at construction.
    // Build a fresh facade so every helper shares the routing lifecycle.
    return new db.constructor(db.client)
  } catch (error) {
    await router.close().catch(() => {})
    throw error
  }
}

const READ_METHODS = new Set(['get', 'cache.get', 'cache.exists'])
const connectionError = (error) => /^(?:NETWORK|IO|CLOSED|CONNECTION_CLOSED|WS_TRANSPORT|GRPC_14|HTTP_503|ECONN|EPIPE|ETIMEDOUT|ERR_HTTP2)/.test(error?.code ?? '')
  || error instanceof TypeError && /fetch failed|network/i.test(error.message)

/** Same routing lifecycle for TCP/TLS, HTTP(S), gRPC(S), and WS(S).
 * Refresh runs before the next operation after 30 seconds or an I/O error.
 * Writes are never replayed, including after a primary change.
 */
export class RoutingClient {
  constructor(seed, client, dial, protocol, { forcePrimary = false, now = Date.now } = {}) {
    this.seed = seed
    this.primary = seed
    this.protocol = protocol
    this.dial = dial
    this.clients = new Map([[seed, Promise.resolve(client)]])
    this.active = new Map()
    this.queues = new Map()
    this.replicas = []
    this.nextReplica = 0
    this.refreshAt = 0
    this.now = now
    this.forcePrimary = forcePrimary
    this.transactionEndpoint = null
    this.sessionEndpoint = null
    this.refreshing = null
    this.closed = false
    this.binaryQueryParams = client.binaryQueryParams === true
  }

  async initialize() {
    await this.refresh()
    return this
  }

  async endpoint(address) {
    if (this.closed) throw new RedDBError('CLOSED', 'connection is closed')
    if (!this.clients.has(address)) {
      const pending = this.dial(address).catch((error) => {
        this.clients.delete(address)
        throw error
      })
      this.clients.set(address, pending)
    }
    return this.clients.get(address)
  }

  async refresh() {
    if (this.transactionEndpoint || this.sessionEndpoint || this.closed) return
    if (this.refreshing) return this.refreshing
    this.refreshing = this.refreshMembership().finally(() => { this.refreshing = null })
    return this.refreshing
  }

  async refreshMembership() {
    this.refreshAt = this.now() + 30_000
    const candidates = [...new Set([this.primary, this.seed, ...this.replicas])]
    for (let index = 0; index < Math.min(candidates.length, 258); index++) {
      const address = candidates[index]
      try {
        const topology = await this.discover(address)
        if (!topology || topology.version !== 1) continue
        const primary = topologyAddress(topology.primary?.addr, this.protocol)
        if (!primary) continue
        // A replica bootstrap tells us where the primary is. Ask that primary
        // for its full authenticated advertisement in the same refresh.
        if (primary !== address && !candidates.includes(primary)) candidates.push(primary)
        this.primary = primary
        this.replicas = (Array.isArray(topology.replicas) ? topology.replicas.slice(0, 256) : [])
          .filter((replica) => replica.healthy === true && replica.rebootstrapping !== true)
          .map((replica) => topologyAddress(replica.addr, this.protocol))
          .filter((replica) => replica && replica !== primary)
        if (primary === address) {
          await this.prune()
          return
        }
      } catch (error) {
        // Old servers have no discovery RPC. Keep the seed usable; the next
        // refresh also probes known replicas if the writer is unavailable.
        if (error?.code === 'HTTP_401' || error?.code === 'GRPC_16') throw error
      }
    }
  }

  async discover(address) {
    const controller = new AbortController()
    let client
    let finished = false
    let timer
    const request = this.dial(address, controller.signal).then(async (transport) => {
      client = transport
      if (finished) { await client.close(); return null }
      return client.call('topology', { transport: this.protocol, signal: controller.signal })
    })
    try {
      return await Promise.race([request, new Promise((_, reject) => {
        timer = setTimeout(() => {
          controller.abort()
          reject(new RedDBError('ETIMEDOUT', 'topology refresh timed out'))
        }, 5000)
      })])
    } finally {
      finished = true
      clearTimeout(timer)
      controller.abort()
      await client?.close()
    }
  }

  async prune() {
    for (const [address, pending] of this.clients) {
      if (address === this.seed || address === this.primary || this.replicas.includes(address)
        || this.active.get(address)) continue
      this.clients.delete(address)
      this.queues.delete(address)
      this.active.delete(address)
      await pending.then((client) => client.close()).catch(() => {})
    }
  }

  async invoke(address, method, params) {
    this.active.set(address, (this.active.get(address) ?? 0) + 1)
    try {
      const client = await this.endpoint(address)
      if (!client.binaryQueryParams) return await client.call(method, params)
      const previous = this.queues.get(address) ?? Promise.resolve()
      const result = previous.then(() => client.call(method, params))
      this.queues.set(address, result.catch(() => {}))
      return await result
    } finally {
      this.active.set(address, this.active.get(address) - 1)
    }
  }

  async call(method, params = {}) {
    if (this.closed) throw new RedDBError('CLOSED', 'connection is closed')
    if (!this.transactionEndpoint && !this.sessionEndpoint && this.now() >= this.refreshAt) await this.refresh()
    const command = method === 'query' ? transactionCommand(params.sql ?? '') : null
    const read = READ_METHODS.has(method) || method === 'query' && isReplicaQuery(params.sql)
    const address = this.transactionEndpoint ?? this.sessionEndpoint ?? (
      read && !this.forcePrimary && this.replicas.length
        ? this.replicas[this.nextReplica++ % this.replicas.length] : this.primary
    )
    if (command === 'BEGIN' || command === 'START TRANSACTION') this.transactionEndpoint = address
    if (method === 'query' && isSessionQuery(params.sql)) this.sessionEndpoint = address
    try {
      const result = await this.invoke(address, method, params)
      if (command === 'COMMIT' || command === 'ROLLBACK') this.transactionEndpoint = null
      return result
    } catch (error) {
      if (!connectionError(error)) throw error
      this.refreshAt = 0
      if (this.transactionEndpoint || this.sessionEndpoint) throw error
      // Only proven reads can be retried. A write may already be committed.
      if (!this.active.get(address)) {
        const failed = this.clients.get(address)
        this.clients.delete(address)
        this.queues.delete(address)
        await failed?.then((client) => client.close()).catch(() => {})
      }
      if (!read) throw error
      this.replicas = this.replicas.filter((replica) => replica !== address)
      await this.refresh()
      return this.invoke(this.primary, method, params)
    }
  }

  async streamSelect(options) {
    const client = await this.endpoint(this.transactionEndpoint ?? this.sessionEndpoint ?? this.primary)
    return client.streamSelect(options)
  }

  async streamInput(options) {
    const client = await this.endpoint(this.transactionEndpoint ?? this.sessionEndpoint ?? this.primary)
    return client.streamInput(options)
  }

  async close() {
    this.closed = true
    const results = await Promise.allSettled([...this.clients.values()].map(async (pending) => {
      const client = await pending
      await client.close()
    }))
    this.clients.clear()
    this.active.clear()
    this.queues.clear()
    const failed = results.find((result) => result.status === 'rejected')
    if (failed) throw failed.reason
  }
}
