import { test } from 'node:test'
import assert from 'node:assert/strict'

import { QueueClient, RedDBError } from '../src/index.js'

function fakeQueue(responder) {
  const calls = []
  const client = {
    async call(method, params) {
      calls.push({ method, params })
      return responder?.(method, params) ?? { rows: [] }
    },
  }
  return { queue: new QueueClient(client), calls }
}

test('queue exposes push SQL for string number JSON object priority key and dedup', async () => {
  const { queue, calls } = fakeQueue()
  await queue.push('jobs', 'hello')
  await queue.push('jobs', 42)
  await queue.push('jobs', { task: 'ship', retries: 2 }, { priority: 7 })
  await queue.push('jobs', { task: 'settle' }, { key: "acct'42", dedup: "retry'1" })

  assert.deepEqual(calls.map((call) => call.params.sql), [
    "QUEUE PUSH jobs 'hello'",
    'QUEUE PUSH jobs 42',
    'QUEUE PUSH jobs {"task":"ship","retries":2} PRIORITY 7',
    `QUEUE PUSH jobs {"task":"settle"} KEY 'acct''42' DEDUP 'retry''1'`,
  ])
})

test('queue pop and peek return payload arrays with expected count clauses', async () => {
  const { queue, calls } = fakeQueue((_, params) => {
    if (params.sql === 'QUEUE POP jobs') return { rows: [{ payload: 'a' }] }
    if (params.sql === 'QUEUE POP jobs COUNT 0') return { rows: [] }
    return { rows: [{ payload: 'b' }, { payload: 'c' }] }
  })

  assert.deepEqual(await queue.pop('jobs'), ['a'])
  assert.deepEqual(await queue.pop('jobs', 0), [])
  assert.deepEqual(await queue.peek('jobs', 2), ['b', 'c'])
  assert.deepEqual(calls.map((call) => call.params.sql), [
    'QUEUE POP jobs',
    'QUEUE POP jobs COUNT 0',
    'QUEUE PEEK jobs COUNT 2',
  ])
})

test('queue len normalizes scalar result and purge returns query result', async () => {
  const { queue, calls } = fakeQueue((_, params) => {
    if (params.sql === 'QUEUE LEN jobs') return { rows: [{ len: 3 }] }
    return { affected: 0, rows: [{ message: "3 messages purged from queue 'jobs'" }] }
  })

  assert.equal(await queue.len('jobs'), 3)
  assert.deepEqual(await queue.purge('jobs'), {
    affected: 0,
    rows: [{ message: "3 messages purged from queue 'jobs'" }],
  })
  assert.deepEqual(calls.map((call) => call.params.sql), [
    'QUEUE LEN jobs',
    'QUEUE PURGE jobs',
  ])
})

test('queue readWait uses the native live wait when the transport supports it', async () => {
  const calls = []
  const queue = new QueueClient({
    nativeQueueWait: true,
    async call(method, params) {
      calls.push({ method, params })
      return {
        rows: [{
          message_id: '42',
          payload: { task: 'ship' },
          consumer: 'worker1',
          delivery_count: 1,
        }],
      }
    },
  })

  assert.deepEqual(
    await queue.readWait('jobs', 'worker1', { waitMs: 30000, group: 'workers' }),
    [{ task: 'ship' }],
  )
  assert.deepEqual(calls, [{
    method: 'queue_wait',
    params: {
      queue: 'jobs',
      group: 'workers',
      consumer: 'worker1',
      count: 1,
      waitMs: 30000,
    },
  }])
})

test('queue readWaitEntries preserves delivery metadata for ACK/NACK', async () => {
  const message = {
    message_id: '42',
    payload: 'work',
    consumer: 'worker1',
    delivery_count: 2,
  }
  const { queue } = fakeQueue(() => ({ rows: [message] }))

  assert.deepEqual(
    await queue.readWaitEntries('jobs', 'worker1', { waitMs: 1000 }),
    [message],
  )
})

test('queue readWait keeps SQL fallback for transports without native wait and COUNT != 1', async () => {
  const calls = []
  const client = {
    nativeQueueWait: true,
    async call(method, params) {
      calls.push({ method, params })
      return { rows: [] }
    },
  }
  const queue = new QueueClient(client)

  await queue.readWait('jobs', 'worker1', { waitMs: 5000, count: 4 })
  client.nativeQueueWait = false
  await queue.readWait('jobs', 'worker1', { waitMs: 5000 })

  assert.deepEqual(calls, [
    {
      method: 'query',
      params: { sql: 'QUEUE READ jobs CONSUMER worker1 COUNT 4 WAIT 5000ms' },
    },
    {
      method: 'query',
      params: { sql: 'QUEUE READ jobs CONSUMER worker1 WAIT 5000ms' },
    },
  ])
})

test('queue readWait requires explicit non-negative integer waitMs', async () => {
  const { queue, calls } = fakeQueue()
  await assert.rejects(
    () => queue.readWait('jobs', 'w', {}),
    (err) => err instanceof RedDBError && err.code === 'INVALID_QUEUE_WAIT',
  )
  await assert.rejects(
    () => queue.readWait('jobs', 'w', { waitMs: -1 }),
    (err) => err instanceof RedDBError && err.code === 'INVALID_QUEUE_WAIT',
  )
  await assert.rejects(
    () => queue.readWait('jobs', 'w', { waitMs: 1.5 }),
    (err) => err instanceof RedDBError && err.code === 'INVALID_QUEUE_WAIT',
  )
  assert.equal(calls.length, 0)
})

test('queue rejects invalid names counts and priorities before query', async () => {
  const { queue, calls } = fakeQueue()
  assert.throws(() => queue.push('bad-name', 'x'), (err) => {
    assert.ok(err instanceof RedDBError)
    assert.equal(err.code, 'INVALID_QUEUE_NAME')
    return true
  })
  await assert.rejects(() => queue.pop('jobs', -1), /non-negative integer/)
  assert.throws(() => queue.push('jobs', 'x', { priority: 1.5 }), /priority must be an integer/)
  assert.equal(calls.length, 0)
})

test('queue rejects ordering key with delayed availability before query', () => {
  const { queue, calls } = fakeQueue()
  assert.throws(
    () => queue.push('jobs', 'x', { key: 'acct-1', delay: '1s' }),
    (err) => {
      assert.ok(err instanceof RedDBError)
      assert.equal(err.code, 'INVALID_ARGUMENT')
      assert.match(err.message, /QUEUE PUSH KEY cannot be combined with DELAY \/ AVAILABLE AT/)
      return true
    },
  )
  assert.throws(
    () => queue.push('jobs', 'x', { key: 'acct-1', at: 1735689600000 }),
    (err) => {
      assert.ok(err instanceof RedDBError)
      assert.equal(err.code, 'INVALID_ARGUMENT')
      return true
    },
  )
  assert.equal(calls.length, 0)
})
