import assert from 'node:assert/strict'
import { once } from 'node:events'
import { createServer } from 'node:http'
import { mkdtemp, readFile, readdir, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, extname, resolve, sep } from 'node:path'
import { fileURLToPath } from 'node:url'
import { after, before, test } from 'node:test'
import { chromium, firefox, webkit } from 'playwright'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..')
const output = resolve(process.env.REDDB_BROWSER_INTEROP_DIR || resolve(root, 'packages/browser/test/interop-build'))
const blazor = resolve(output, 'dotnet/bin/BrowserHarness/debug/wwwroot')
const browserTypes = { chromium, firefox, webkit }
const browsers = (process.env.REDDB_BROWSER_BROWSERS || 'chromium,firefox,webkit').split(',')
let server
let origin
let temporary
let requests = 0

async function files(directory, prefix) {
  const names = []
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const url = `${prefix}/${entry.name}`
    if (entry.isDirectory()) names.push(...await files(resolve(directory, entry.name), url))
    else names.push(url)
  }
  return names
}

const dartPage = `<!doctype html><meta charset="utf-8"><title>Dart WASM interop conformance</title>
<script>window.probeResult = new Promise((resolve, reject) => {
  window.probeComplete = resolve; window.probeFailed = reject
})</script>
<script type="module">
  import * as module from '/packages/browser/src/index.js'
  globalThis.reddbBrowserModule = module
  const script = document.createElement('script')
  script.src = './main.js'
  script.onerror = () => probeFailed('Dart script failed to load')
  document.head.append(script)
</script>`

before(async () => {
  temporary = await mkdtemp(resolve(tmpdir(), 'reddb-interop-'))
  const assets = [
    '/interop/dart/', '/interop/dart/main.js', '/interop/blazor/',
    '/packages/browser/src/index.js', '/packages/browser/src/protocol.js',
    '/packages/browser/src/worker.js', '/packages/browser/src/wasm/reddb_wasm.js',
    '/packages/browser/src/wasm/reddb_wasm_bg.wasm',
    ...await files(resolve(blazor, '_framework'), '/interop/blazor/_framework'),
  ]
  const offline = `const assets = ${JSON.stringify(assets)}
self.addEventListener('install', event => event.waitUntil(caches.open('interop-v1').then(cache => cache.addAll(assets))))
self.addEventListener('activate', event => event.waitUntil(self.clients.claim()))
self.addEventListener('fetch', event => event.respondWith(caches.open('interop-v1').then(async cache => await cache.match(event.request) || Response.error())))`
  server = createServer(async (request, response) => {
    requests++
    const url = decodeURIComponent(new URL(request.url, 'http://localhost').pathname)
    try {
      if (url === '/interop/dart/') return response.writeHead(200, { 'Content-Type': 'text/html' }).end(dartPage)
      if (url === '/offline.js') return response.writeHead(200, { 'Content-Type': 'text/javascript' }).end(offline)
      let base = root
      let relative = `.${url}`
      if (url === '/interop/blazor/') relative = 'drivers/dotnet/tests/BrowserHarness/wwwroot/index.html'
      else if (url.startsWith('/interop/blazor/')) { base = blazor; relative = url.slice('/interop/blazor/'.length) }
      else if (url === '/interop/dart/main.js') { base = output; relative = 'dart/main.js' }
      const path = resolve(base, relative)
      if (!path.startsWith(`${base}${sep}`)) return response.writeHead(403).end()
      const type = { '.html': 'text/html', '.js': 'text/javascript', '.wasm': 'application/wasm', '.json': 'application/json' }[extname(path)] || 'application/octet-stream'
      const body = await readFile(path)
      response.writeHead(200, { 'Content-Type': type }).end(body)
    } catch { response.writeHead(404).end() }
  })
  server.listen(0, '127.0.0.1')
  await once(server, 'listening')
  origin = `http://127.0.0.1:${server.address().port}`
})

after(async () => {
  if (server?.listening) await new Promise(resolve => server.close(resolve))
  if (temporary) await rm(temporary, { recursive: true, force: true })
})

for (const name of browsers) {
  test(`${name}: compiled Dart and Blazor use the shared WASM`, { timeout: 180_000 }, async testContext => {
    const profile = resolve(temporary, name)
    // WPE's persistent-profile runner returns empty CacheStorage even after a
    // successful put. Its ephemeral context supports real cache/offline reloads.
    const ephemeralBrowser = name === 'webkit' ? await webkit.launch({ headless: true }) : undefined
    let context = ephemeralBrowser
      ? await ephemeralBrowser.newContext()
      : await browserTypes[name].launchPersistentContext(profile, { headless: true })
    async function probe(language) {
      const page = await context.newPage()
      const diagnostics = []
      page.on('pageerror', error => diagnostics.push(error.message))
      page.on('console', message => { if (message.type() === 'error') diagnostics.push(message.text()) })
      try {
        await page.goto(`${origin}/interop/${language}/`)
        await page.evaluate(() => {
          window.probeResult.then(
            result => { window.probeOutcome = { result } },
            error => { window.probeOutcome = { error: String(error) } },
          )
        })
        await page.waitForFunction(() => window.probeOutcome, { }, { timeout: 60_000 })
        const outcome = await page.evaluate(() => window.probeOutcome)
        assert.equal(outcome.error, undefined, outcome.error)
        assert.deepEqual(outcome.result, {
          language, abi: 1, storedQueries: true, answer: 42, exact: '9007199254740993',
          unsupported: 'QUERY_UNSUPPORTED', closed: 'CLOSED',
        })
        return page
      } catch (error) {
        await page.close()
        throw new Error(`${name}/${language}: ${error.message}\n${diagnostics.join('\n')}`, { cause: error })
      }
    }
    try {
      const first = await probe('dart')
      await (await probe('blazor')).close()
      await first.evaluate(() => navigator.serviceWorker.register('/offline.js', { scope: '/' }))
      await first.waitForFunction(() => navigator.serviceWorker.controller)
      const { port } = server.address()
      await new Promise(resolve => server.close(resolve))
      try {
        const before = requests
        async function offlineProbes() {
          for (const language of ['dart', 'blazor']) {
            const page = await probe(language)
            if (name !== 'webkit') assert.equal(await page.evaluate(() => navigator.onLine), false)
            await page.close()
          }
          assert.equal(requests, before, 'offline adapters must not contact the HTTP host')
        }
        await testContext.test('fresh Workers execute with the HTTP host stopped', async () => {
          // WPE's simulated offline flag fails navigation before its SW can
          // respond. Its cache is exercised with the HTTP listener stopped.
          if (name !== 'webkit') await context.setOffline(true)
          await offlineProbes()
        })
        await testContext.test('complete browser restart works offline', {
          // WPE persistent profiles do not store CacheStorage entries.
          skip: name === 'webkit' ? 'Playwright WPE persistent profiles lack working CacheStorage; real Safari/iOS qualification remains pending' : false,
        }, async () => {
          await context.close()
          context = await browserTypes[name].launchPersistentContext(profile, { headless: true, offline: true })
          await offlineProbes()
        })
      } finally {
        server.listen(port, '127.0.0.1')
        await once(server, 'listening')
      }
    } finally { await context.close(); await ephemeralBrowser?.close() }
  })
}
