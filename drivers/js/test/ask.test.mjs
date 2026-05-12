import { test } from 'node:test'
import assert from 'node:assert/strict'

import { RedDB } from '../src/index.js'

test('db.query ASK round-trips the full response object', async () => {
  const ask = {
    answer: 'Deploy failed [^1].',
    cache_hit: false,
    citations: [{ marker: 1, urn: 'urn:reddb:row:deployments:1' }],
    completion_tokens: 7,
    cost_usd: 0,
    mode: 'strict',
    model: 'gpt-4o-mini',
    prompt_tokens: 11,
    provider: 'openai',
    retry_count: 0,
    sources_flat: [
      {
        urn: 'urn:reddb:row:deployments:1',
        payload: '{"collection":"deployments","id":"1","kind":"row"}',
      },
    ],
    validation: { ok: true, warnings: [], errors: [] },
  }
  const calls = []
  const db = new RedDB({
    call(method, params) {
      calls.push({ method, params })
      return Promise.resolve(ask)
    },
  })

  const result = await db.query("ASK 'why did deploy fail?'")

  assert.equal(calls.length, 1)
  assert.deepEqual(calls[0], {
    method: 'query',
    params: { sql: "ASK 'why did deploy fail?'" },
  })
  assert.deepEqual(result, ask)
  assert.equal(result.citations[0].urn, 'urn:reddb:row:deployments:1')
  assert.equal(result.sources_flat[0].payload, '{"collection":"deployments","id":"1","kind":"row"}')
  assert.equal(result.validation.ok, true)
})
