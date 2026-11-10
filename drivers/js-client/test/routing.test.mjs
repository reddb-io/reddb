import { test } from 'node:test'
import assert from 'node:assert/strict'
import { RoutingClient } from '../src/core/routing.js'
import { isReplicaQuery, transactionCommand, topologyAddress, decodeTopology } from '../src/core/topology.js'

for (const protocol of ['red', 'reds', 'grpc', 'grpcs', 'http', 'https', 'ws', 'wss']) {
  test(`${protocol}: discovers replicas, routes SQL mutations and pins transactions`, async () => {
    const primary = `${protocol}://primary:5000`
    const replica = `${protocol}://replica:6000`
    const calls = []
    let now = 1
    let replicas = [{ addr: replica, healthy: true }]
    let advertisedPrimary = primary
    const dial = async (address) => ({
      binaryQueryParams: protocol === 'red',
      async call(method, params) {
        calls.push([address, method, params.sql])
        if (method === 'topology') return { version: 1, primary: { addr: advertisedPrimary }, replicas }
        return address
      },
      async close() {},
    })
    const client = await new RoutingClient(primary, await dial(primary), dial, protocol, { now: () => now }).initialize()
    assert.equal(await client.call('query', { sql: '/* read */ SELECT COUNT(*) FROM users' }), replica)
    assert.equal(await client.call('query', { sql: 'UPDATE users SET name = $1', params: ['name'] }), primary)
    assert.equal(await client.call('insert', {}), primary)
    await client.call('query', { sql: '/* transaction */ BEGIN' })
    assert.equal(await client.call('query', { sql: 'SELECT * FROM users' }), primary)
    await client.call('query', { sql: 'ROLLBACK TO SAVEPOINT nested' })
    assert.equal(await client.call('query', { sql: 'SELECT * FROM users' }), primary)
    await client.call('query', { sql: 'COMMIT' })
    assert.equal(await client.call('get', {}), replica)
    replicas = []
    now += 30_001
    assert.equal(await client.call('get', {}), primary)
    advertisedPrimary = `${protocol}://promoted:6000`
    now += 30_001
    assert.equal(await client.call('insert', {}), advertisedPrimary)
    await client.close()
    await assert.rejects(client.call('get', {}), { code: 'CLOSED' })
  })
}

test('read failure falls back; an ambiguous write is never replayed', async () => {
  const primary = 'http://primary:5000'
  const replica = 'http://replica:5000'
  let writes = 0
  let reads = 0
  const dial = async (address) => ({
    async call(method) {
      if (method === 'topology') return {
        version: 1, primary: { addr: primary }, replicas: [{ addr: replica, healthy: true }],
      }
      if (method === 'insert') { writes++; throw Object.assign(new Error('lost commit reply'), { code: 'ECONNRESET' }) }
      reads++
      if (address === replica) throw Object.assign(new Error('disconnected'), { code: 'ECONNRESET' })
      return 'primary read'
    },
    async close() {},
  })
  const client = await new RoutingClient(primary, await dial(primary), dial, 'http').initialize()
  assert.equal(await client.call('get', {}), 'primary read')
  assert.equal(reads, 2)
  await assert.rejects(client.call('insert', {}), /lost commit reply/)
  assert.equal(writes, 1)
  await client.close()
})

test('TLS, unhealthy and rebootstrap replicas are excluded', async () => {
  const primary = 'https://primary:5000'
  const transport = {
    async call(method) {
      if (method === 'topology') return {
        version: 1, primary: { addr: primary }, replicas: [
          { addr: 'http://plaintext:5000', healthy: true },
          { addr: 'https://unhealthy:5000', healthy: false },
          { addr: 'https://rebootstrap:5000', healthy: true, rebootstrapping: true },
        ],
      }
      return 'primary'
    },
    async close() {},
  }
  const client = await new RoutingClient(primary, transport, () => assert.fail('must not dial'), 'https').initialize()
  assert.equal(await client.call('get', {}), 'primary')
  assert.equal(topologyAddress('https://user:secret@host', 'https'), null)
  await client.close()
})

test('legacy server without discovery keeps a working seed connection', async () => {
  const transport = {
    async call(method) {
      if (method === 'topology') throw Object.assign(new Error('unknown route'), { code: 'HTTP_404' })
      return 'legacy result'
    },
    async close() {},
  }
  const client = await new RoutingClient('http://seed:5000', transport, () => assert.fail(), 'http').initialize()
  assert.equal(await client.call('query', { sql: 'SELECT 1' }), 'legacy result')
  await client.close()
})

test('SQL classification is conservative around comments, statements and functions', () => {
  for (const sql of ["SELECT 'UPDATE; DELETE' AS text", '/* a /* b */ c */ SELECT * FROM users;', 'SELECT COUNT(*) FROM users']) {
    assert.equal(isReplicaQuery(sql), true, sql)
  }
  for (const sql of ['SELECT 1; DELETE FROM users', 'SELECT * FROM users FOR UPDATE',
    "WITH changed AS (DELETE FROM users RETURNING *) SELECT * FROM changed", 'SELECT nextval(\'counter\')',
    'EXPLAIN ANALYZE DELETE FROM users', "SELECT 'unterminated", '/* unterminated SELECT 1']) {
    assert.equal(isReplicaQuery(sql), false, sql)
  }
  assert.equal(transactionCommand('/* begin */ BEGIN TRANSACTION;'), 'BEGIN')
  assert.equal(transactionCommand('ROLLBACK TO SAVEPOINT point'), null)
})

test('topology decoder rejects corrupt payloads and tolerates future versions', () => {
  assert.throws(() => decodeTopology(new Uint8Array([1, 0])))
  assert.equal(decodeTopology(new Uint8Array([2, 0, 0, 0, 0])), null)
})

test('new streams refresh membership and failed input is never replayed', async () => {
  let now = 1
  let primary = 'http://old:5000'
  let writes = 0
  const opened = []
  const dial = async (address) => ({
    async call() { return { version: 1, primary: { addr: primary }, replicas: [] } },
    async streamSelect() {
      opened.push(address)
      return {
        async *[Symbol.asyncIterator]() { yield address },
        async cancel() {},
      }
    },
    async streamInput() {
      opened.push(address)
      return {
        async write() { writes++; throw Object.assign(new Error('lost input'), { code: 'ECONNRESET' }) },
        async cancel() {},
      }
    },
    async close() {},
  })
  const client = await new RoutingClient(primary, await dial(primary), dial, 'http', { now: () => now }).initialize()
  const original = await client.streamSelect({})
  primary = 'http://promoted:5000'
  now += 30_001
  const input = await client.streamInput({})
  assert.equal(opened.at(-1), primary)
  await assert.rejects(input.write({ value: 1 }), /lost input/)
  assert.equal(writes, 1)
  primary = 'http://next:5000'
  const next = await client.streamSelect({})
  assert.equal(opened.at(-1), primary)
  assert.deepEqual(await Array.fromAsync(original), ['http://old:5000'])
  assert.deepEqual(await Array.fromAsync(next), [primary])
  await client.close()
})

test('transaction streams hold the pinned binary connection until completion', async () => {
  const primary = 'red://primary:5050'
  const transport = {
    binaryQueryParams: true,
    async call(method) {
      if (method === 'topology') return { version: 1, primary: { addr: primary }, replicas: [] }
      return 'query'
    },
    async streamSelect() {
      return { async *[Symbol.asyncIterator]() { yield 'row' }, async cancel() {} }
    },
    async close() {},
  }
  const client = await new RoutingClient(primary, transport, async () => transport, 'red').initialize()
  await client.call('query', { sql: 'BEGIN' })
  const stream = await client.streamSelect({})
  let resolved = false
  const pending = client.call('query', { sql: 'SELECT 1' }).then(() => { resolved = true })
  await new Promise((resolve) => setTimeout(resolve, 10))
  assert.equal(resolved, false)
  assert.deepEqual(await Array.fromAsync(stream), ['row'])
  await pending
  await client.close()
})
