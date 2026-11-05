import { execFileSync } from 'node:child_process'
import { createServer } from 'node:http'
import { readFile, writeFile } from 'node:fs/promises'
import { resolve, sep } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { cpus, platform, arch } from 'node:os'
import { measureFrames } from './frames.js'

const root = resolve(fileURLToPath(new URL('../../../', import.meta.url)))
const baselineRoot = resolve(process.env.REDDB_BENCH_BASELINE || root)
const options = {
  rounds: Number(process.env.REDDB_BENCH_ROUNDS || 20),
  sampleMs: Number(process.env.REDDB_BENCH_SAMPLE_MS || 20),
}
const load = (directory) => import(pathToFileURL(resolve(directory, 'drivers/js-client/src/redwire-core.js')))
const report = {
  capturedAt: new Date().toISOString(),
  host: { platform: platform(), arch: arch(), cpu: cpus()[0]?.model },
  head: execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim(),
  baseline: execFileSync('git', ['rev-parse', 'HEAD'], { cwd: baselineRoot, encoding: 'utf8' }).trim(),
  method: 'Scripted socket events in actual runtimes; receive accumulation, decode and Promise delivery. Prebuilt input, handshake and byte-parity checks are outside timings. Copy counts instrument Uint8Array.set separately, excluding decoder payload slices; they are not peak memory. No network, WebSocket delivery or database execution is measured.',
  node: { version: process.version, ...await measureFrames(await load(baselineRoot), await load(root), options) },
}
const server = createServer(async (request, response) => {
  if (request.url === '/') { response.setHeader('Content-Type', 'text/html'); response.end('<!doctype html><title>Frame reader benchmark</title>'); return }
  try {
    const url = new URL(request.url, 'http://localhost')
    const directory = url.pathname.startsWith('/baseline/') ? baselineRoot : root
    const path = resolve(directory, `.${url.pathname.replace(/^\/(baseline|current)/, '')}`)
    if (!path.startsWith(directory + sep)) { response.writeHead(403).end(); return }
    response.setHeader('Content-Type', 'text/javascript')
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
        const { measureFrames } = await import('/current/drivers/js-client/bench/frames.js')
        const baseline = await import('/baseline/drivers/js-client/src/redwire-core.js')
        const candidate = await import('/current/drivers/js-client/src/redwire-core.js')
        return { userAgent: navigator.userAgent, ...await measureFrames(baseline, candidate, options) }
      }, options)
    } finally { await browser.close() }
  }
} finally { await new Promise((resolve) => server.close(resolve)) }
if (process.env.REDDB_BENCH_OUTPUT) await writeFile(process.env.REDDB_BENCH_OUTPUT, JSON.stringify(report, null, 2) + '\n')
console.log(JSON.stringify(report, null, 2))
