import { createHash } from 'node:crypto'
import { readdir, readFile, writeFile } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { build } from 'vite'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const output = resolve(root, 'dist')
await build({ root, base: './', build: { outDir: output, emptyOutDir: true } })
async function files(directory, prefix = './') {
  const names = []
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const name = `${prefix}${entry.name}`
    if (entry.isDirectory()) names.push(...await files(resolve(directory, entry.name), `${name}/`))
    else names.push(name)
  }
  return names.sort()
}
const assets = await files(output)
const hash = createHash('sha256').update(await readFile(fileURLToPath(import.meta.url)))
for (const path of assets) hash.update(path).update(await readFile(resolve(output, path)))
const revision = hash.digest('hex').slice(0, 16)
await writeFile(resolve(output, 'service-worker.js'), `
const prefix = 'reddb-browser-example:' + self.registration.scope + ':'
const current = prefix + '${revision}'
const assets = ${JSON.stringify(['./', ...assets])}
const requests = assets.map(path => new Request(new URL(path, self.registration.scope), { cache: 'reload' }))
self.addEventListener('install', event => event.waitUntil(caches.open(current).then(cache => cache.addAll(requests))))
self.addEventListener('message', event => {
  if (event.data?.type === 'ACTIVATE_UPDATE') event.waitUntil(self.skipWaiting())
})
self.addEventListener('activate', event => event.waitUntil((async () => {
  for (const name of await caches.keys()) if (name.startsWith(prefix) && name !== current) await caches.delete(name)
  await self.clients.claim()
})()))
self.addEventListener('fetch', event => {
  if (event.request.method !== 'GET' || !event.request.url.startsWith(self.registration.scope)) return
  event.respondWith(caches.open(current).then(async cache => await cache.match(event.request, { ignoreSearch: event.request.mode === 'navigate' }) || fetch(event.request)))
})
`)
