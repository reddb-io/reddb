// Test host's application cache. The SDK never registers a service worker or
// takes ownership of the embedding application's cache/update policy.
const cacheName = 'reddb-browser-offline-test-v1'
const assets = [
  './harness.html',
  '../src/index.js',
  '../src/worker.js',
  '../src/wasm/reddb_wasm.js',
  '../src/wasm/reddb_wasm_bg.wasm',
].map((path) => new URL(path, self.location.href).href)

self.addEventListener('install', (event) => {
  event.waitUntil(caches.open(cacheName).then((cache) => cache.addAll(assets)))
})

self.addEventListener('activate', (event) => {
  event.waitUntil(self.clients.claim())
})

self.addEventListener('fetch', (event) => {
  if (event.request.method !== 'GET' || !assets.includes(event.request.url)) return
  event.respondWith(caches.open(cacheName).then(async (cache) => {
    return await cache.match(event.request) || Response.error()
  }))
})
