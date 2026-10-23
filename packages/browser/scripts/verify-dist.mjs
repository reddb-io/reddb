import { readFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const directory = resolve(dirname(fileURLToPath(import.meta.url)), '..')
for (const path of ['src/index.js', 'src/index.d.ts', 'src/worker.js', 'src/wasm/reddb_wasm.js', 'LICENSE']) {
  if (!readFileSync(resolve(directory, path)).length) throw new Error(`Missing browser distribution file: ${path}`)
}
const wasm = readFileSync(resolve(directory, 'src/wasm/reddb_wasm_bg.wasm'))
if (wasm.length < 8 || !wasm.subarray(0, 8).equals(Buffer.from([0, 97, 115, 109, 1, 0, 0, 0]))) {
  throw new Error('Build the browser WASM before packing: pnpm --filter @reddb-io/browser build')
}
