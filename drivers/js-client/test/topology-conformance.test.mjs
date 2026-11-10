import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { decodeTopology, TOPOLOGY_TRANSPORT_METADATA_KEY } from '../src/core/topology.js'

const manifest = JSON.parse(await readFile(
  new URL('../../../testdata/conformance/topology/manifest.json', import.meta.url), 'utf8'))

test('topology metadata selector matches the canonical wire contract', () => {
  assert.equal(TOPOLOGY_TRANSPORT_METADATA_KEY, manifest.metadata_key)
})

for (const fixture of manifest.cases) {
  test(`shared topology fixture: ${fixture.name}`, () => {
    const bytes = Buffer.from(fixture.hex, 'hex')
    if (fixture.error) assert.throws(() => decodeTopology(bytes))
    else assert.deepEqual(decodeTopology(bytes), fixture.expected)
  })
}
