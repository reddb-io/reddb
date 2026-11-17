// Repro do wedge de execução órfã (incidente stg 2026-11-17, platform-db-0).
// Padrão do svc-api: conexões RedWire persistentes emitindo KV PUT em loop,
// com clientes que desconectam abruptamente no meio de execuções.
// Uso: node repro.mjs <host> <wirePort> [durSecs]
import { connectRedwire } from '../../drivers/js-client/src/redwire.js'

const host = process.argv[2] ?? '127.0.0.1'
const port = Number(process.argv[3] ?? 15050)
const httpBase = `http://${host}:15000`
const durSecs = Number(process.argv[4] ?? 600)

const PUTS_PER_CONNECTION = 40
const CONNECTION_DELAY_MS = 150
const KEY = "stg:api-queue-worker:organization-usage-delivery"
const sleep = ms => new Promise(r => setTimeout(r, ms))

async function healthReady() {
  try {
    const res = await fetch(`${httpBase}/health/ready`, { signal: AbortSignal.timeout(4000) })
    return { status: res.status, body: (await res.text()).slice(0, 160) }
  } catch (e) { return { status: 'ERR', body: String(e).slice(0, 100) } }
}

async function workerWorker(id, stats) {
  let client
  try { client = await connectRedwire({ host, port, clientName: `w${id}` }) }
  catch { stats.connectFails++; return }
  try {
    for (let i = 0; i < PUTS_PER_CONNECTION; i++) {
      if (i === PUTS_PER_CONNECTION / 2 && Math.random() < 0.35) {
        stats.abruptDrops++; client.socket.destroy(); return
      }
      const payload = JSON.stringify({ outcome: 'ok', worker: id, seq: i, ts: Date.now() })
      const t0 = Date.now()
      await client.call('query', { sql: `KV PUT '${KEY}' = '${payload}'` })
      stats.putMs.push(Date.now() - t0); stats.puts++
    }
    stats.cleanClose++; client.socket.end()
  } catch (e) { stats.errors++; try { client.socket.destroy() } catch {} }
}

const stats = { puts: 0, errors: 0, abruptDrops: 0, cleanClose: 0, connectFails: 0, putMs: [] }
console.log(`repro: ${durSecs}s contra ${host}:${port}`)
const start = Date.now(); let seq = 0
while ((Date.now() - start) / 1000 < durSecs) {
  const batch = []
  for (let i = 0; i < 6; i++) batch.push(workerWorker(seq++, stats))
  await Promise.allSettled(batch)
  await sleep(CONNECTION_DELAY_MS)
  if (seq % 60 === 0) {
    const h = await healthReady()
    const recent = stats.putMs.slice(-20)
    const p50 = recent.length ? [...recent].sort((a, b) => a - b)[10] : -1
    console.log(`t=${Math.round((Date.now() - start) / 1000)}s puts=${stats.puts} drops=${stats.abruptDrops} err=${stats.errors} putP50=${p50}ms ready=${h.status}`)
    if (p50 > 5000 && h.status === 200) { console.log('*** WEDGE: p50>5s com ready=200'); break }
  }
}
const final = await healthReady()
console.log(`final: puts=${stats.puts} drops=${stats.abruptDrops} err=${stats.errors} ready=${JSON.stringify(final)}`)
const t0 = Date.now()
try {
  const res = await fetch(`${httpBase}/query`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: '{"sql":"SELECT 1"}', signal: AbortSignal.timeout(35000) })
  console.log(`probe SELECT 1: HTTP ${res.status} em ${Date.now() - t0}ms: ${(await res.text()).slice(0, 180)}`)
} catch (e) { console.log(`probe SELECT 1: ERRO após ${Date.now() - t0}ms: ${String(e).slice(0, 90)}`) }
