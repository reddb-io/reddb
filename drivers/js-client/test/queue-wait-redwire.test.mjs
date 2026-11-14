import test from 'node:test'
import assert from 'node:assert/strict'

import {
  decodeFrame,
  MessageKind,
  RedWireClient,
} from '../src/redwire-core.js'

const encodeJson = (value) => new TextEncoder().encode(JSON.stringify(value))

function fakeTransport(response) {
  const sent = []
  const client = new RedWireClient(
    {
      write(frame, callback) { sent.push(frame); callback() },
    },
    {
      async next() { return response },
    },
    '',
    0,
  )
  return { client, sent }
}

test('RedWire native queue wait sends QueueWaitOpen and returns the pushed message', async () => {
  const message = {
    message_id: '42',
    payload: { task: 'ship' },
    consumer: 'worker1',
    delivery_count: 1,
  }
  const { client, sent } = fakeTransport({
    kind: MessageKind.QueueEventPush,
    correlationId: 1n,
    streamId: 1,
    payload: encodeJson(message),
  })

  assert.equal(client.nativeQueueWait, true)
  assert.deepEqual(await client.call('queue_wait', {
    queue: 'jobs',
    group: 'workers',
    consumer: 'worker1',
    count: 1,
    waitMs: 30000,
  }), { rows: [message] })

  assert.equal(sent.length, 1)
  const frame = decodeFrame(sent[0])
  assert.equal(frame.kind, MessageKind.QueueWaitOpen)
  assert.equal(frame.correlationId, 1n)
  assert.equal(frame.streamId, 1)
  assert.deepEqual(JSON.parse(new TextDecoder().decode(frame.payload)), {
    queue: 'jobs',
    group: 'workers',
    consumer: 'worker1',
    count: 1,
    wait_ms: 30000,
  })
})

test('RedWire native queue wait maps QueueWaitTimeout to an empty result', async () => {
  const { client } = fakeTransport({
    kind: MessageKind.QueueWaitTimeout,
    correlationId: 1n,
    streamId: 1,
    payload: encodeJson({ outcome: 'timeout', queue: 'jobs', wait_ms: 100 }),
  })

  assert.deepEqual(await client.call('queue_wait', {
    queue: 'jobs',
    consumer: 'worker1',
    count: 1,
    waitMs: 100,
  }), { rows: [] })
})

test('RedWire native queue wait preserves structured server errors', async () => {
  const { client } = fakeTransport({
    kind: MessageKind.StreamError,
    correlationId: 1n,
    streamId: 1,
    payload: encodeJson({
      code: 'queue_wait_exceeds_cap',
      message: 'requested wait exceeds queue.max_wait_ms',
    }),
  })

  await assert.rejects(
    () => client.call('queue_wait', {
      queue: 'jobs',
      consumer: 'worker1',
      count: 1,
      waitMs: 3600000,
    }),
    (error) => error.code === 'queue_wait_exceeds_cap'
      && /queue\.max_wait_ms/.test(error.message),
  )
})

test('RedWire native queue wait rejects a mismatched stream response', async () => {
  const { client } = fakeTransport({
    kind: MessageKind.QueueWaitTimeout,
    correlationId: 1n,
    streamId: 2,
    payload: encodeJson({ outcome: 'timeout' }),
  })

  await assert.rejects(
    () => client.call('queue_wait', {
      queue: 'jobs',
      consumer: 'worker1',
      count: 1,
      waitMs: 100,
    }),
    (error) => error.code === 'QUEUE_WAIT_PROTOCOL',
  )
})
