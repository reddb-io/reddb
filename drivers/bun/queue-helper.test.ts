import { expect, test } from 'bun:test'
import { RedDBConnection } from './index'

test('queue helper delegates renewable leases to the Bun transport', async () => {
  const connection = new RedDBConnection({})
  const calls: string[] = []
  ;(connection as any).query = async (sql: string) => {
    calls.push(sql)
    return JSON.stringify({ affected: 1, rows: [] })
  }

  await connection.queue('jobs').extend('d1')
  expect(calls).toEqual(["QUEUE EXTEND jobs WITH delivery_id = 'd1'"])
})

test('queue helper exposes the shared managed consumer', () => {
  const connection = new RedDBConnection({})
  const consumer = connection.queue('jobs').consumer({
    consumer: 'worker',
    handleMessage: async () => {},
    concurrency: 2,
    leaseHeartbeatMs: 1000,
  })

  expect(consumer.status).toEqual({
    state: 'idle',
    isRunning: false,
    isPaused: false,
    inFlight: 0,
    concurrency: 2,
  })
})
