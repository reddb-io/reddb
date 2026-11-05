import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { readFile } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const repository = resolve(dirname(fileURLToPath(import.meta.url)), '../../..')
const manifest = JSON.parse(await readFile(resolve(repository, 'design-system.manifest.json'), 'utf8'))
const vendor = resolve(repository, manifest.dest)
const receipt = JSON.parse(await readFile(resolve(vendor, 'design-system.receipt.json'), 'utf8'))
assert.equal(receipt.version, manifest.version)
assert.equal(receipt.framework, manifest.framework)
assert.deepEqual(receipt.kits, manifest.kits)
for (const [file, expected] of Object.entries(receipt.files)) {
  const bytes = await readFile(resolve(vendor, file))
  assert.equal(createHash('sha256').update(bytes).digest('hex'), expected, `DS receipt mismatch: ${file}`)
}
console.log(`Design System ${receipt.version}: ${Object.keys(receipt.files).length} file hashes verified (${receipt.revision})`)
