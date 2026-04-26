/**
 * End-to-end smoke for the JS driver's RedWire v2 transport.
 *
 * Skipped automatically when the engine binary isn't running on
 * REDWIRE_TEST_HOST:REDWIRE_TEST_PORT (default 127.0.0.1:5050).
 * In CI, spin up the binary via:
 *
 *   cargo run --release --bin red --
 *     server --bind 127.0.0.1:5050
 *
 * The 0xFE dispatch in src/wire/listener.rs routes this driver's
 * connection to the v2 session.
 */

import assert from 'node:assert/strict'
import { connectRedwire } from '../src/redwire.js'

const HOST = process.env.REDWIRE_TEST_HOST ?? '127.0.0.1'
const PORT = Number(process.env.REDWIRE_TEST_PORT ?? '5050')
const SHOULD_RUN = process.env.REDWIRE_E2E === '1'

if (!SHOULD_RUN) {
  console.log('redwire e2e: skipped (set REDWIRE_E2E=1 + start the engine on :5050)')
  process.exit(0)
}

const client = await connectRedwire({
  host: HOST,
  port: PORT,
  auth: { kind: 'anonymous' },
  clientName: 'redwire-smoke-mjs',
})

const result = await client.call('query', { sql: 'SELECT 1' })
assert.equal(typeof result.statement, 'string', 'server populated statement')

const ping = await client.call('health', {})
assert.equal(ping.ok, true)

await client.close()
console.log('redwire e2e: pass')
