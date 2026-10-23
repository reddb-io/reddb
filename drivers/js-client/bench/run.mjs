import { execFileSync } from 'node:child_process'
import { createServer } from 'node:http'
import { readFile, writeFile } from 'node:fs/promises'
import { resolve, extname, sep } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { gzipSync } from 'node:zlib'
import { cpus, platform, arch } from 'node:os'
import { measureCodecs } from './codecs.js'

const root = resolve(fileURLToPath(new URL('../../../', import.meta.url)))
const baselineRoot = resolve(process.env.REDDB_BENCH_BASELINE || root)
const output = process.env.REDDB_BENCH_OUTPUT
const options = {
  rounds: Number(process.env.REDDB_BENCH_ROUNDS || 20),
  sampleMs: Number(process.env.REDDB_BENCH_SAMPLE_MS || 20),
}
const load = async (directory) => ({
  ...await import(pathToFileURL(resolve(directory, 'drivers/js-client/src/redwire-core.js'))),
  ...await import(pathToFileURL(resolve(directory, 'drivers/js-client/src/core/serialization.js'))),
})
const wasmPath = resolve(root, 'drivers/js-client/bench/wasm/client_codec_bg.wasm')
const bytes = await readFile(wasmPath)
const start = performance.now()
const wasm = await import('./wasm/client_codec.js')
const instance = await wasm.default({ module_or_path: bytes })
const initializationMs = performance.now() - start
const report = {
  capturedAt: new Date().toISOString(),
  host: { platform: platform(), arch: arch(), cpu: cpus()[0]?.model },
  head: execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim(),
  baseline: execFileSync('git', ['rev-parse', 'HEAD'], { cwd: baselineRoot, encoding: 'utf8' }).trim(),
  wasm: { bytes: bytes.length, gzipBytes: gzipSync(bytes).length },
  node: { version: process.version, initializationMs, ...await measureCodecs(await load(baselineRoot), await load(root), wasm, options), linearMemoryBytes: instance.memory.buffer.byteLength },
}
const server = createServer(async (request, response) => {
  if (request.url === '/') { response.setHeader('Content-Type', 'text/html'); response.end('<!doctype html><title>Client codec benchmark</title>'); return }
  try {
    const url = new URL(request.url, 'http://localhost')
    const baseline = url.pathname.startsWith('/baseline/')
    const directory = baseline ? baselineRoot : root
    const path = resolve(directory, `.${url.pathname.replace(/^\/(baseline|current)/, '')}`)
    if (!path.startsWith(directory + sep)) { response.writeHead(403).end(); return }
    response.setHeader('Content-Type', extname(path) === '.wasm' ? 'application/wasm' : 'text/javascript')
    response.end(await readFile(path))
  } catch { response.writeHead(404).end() }
})
try {
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
  const origin = `http://127.0.0.1:${server.address().port}`
  const playwright = await import('../../../packages/browser/node_modules/playwright/index.mjs')
  for (const name of (process.env.REDDB_BENCH_BROWSERS || 'chromium,firefox,webkit').split(',')) {
    const browser = await playwright[name].launch({ headless: true })
    try {
      const page = await browser.newPage()
      await page.goto(origin)
      report[name] = await page.evaluate(async (options) => {
        const load = async (prefix) => ({
          ...await import(`/${prefix}/drivers/js-client/src/redwire-core.js`),
          ...await import(`/${prefix}/drivers/js-client/src/core/serialization.js`),
        })
        const { measureCodecs } = await import('/current/drivers/js-client/bench/codecs.js')
        const baseline = await load('baseline')
        const candidate = await load('current')
        const start = performance.now()
        const wasm = await import('/current/drivers/js-client/bench/wasm/client_codec.js')
        const instance = await wasm.default()
        const initializationMs = performance.now() - start
        return { userAgent: navigator.userAgent, initializationMs, ...await measureCodecs(baseline, candidate, wasm, options), linearMemoryBytes: instance.memory.buffer.byteLength }
      }, options)
    } finally { await browser.close() }
  }
} finally { await new Promise((resolve) => server.close(resolve)) }
if (output) await writeFile(output, JSON.stringify(report, null, 2) + '\n')
console.log(JSON.stringify(report, null, 2))
