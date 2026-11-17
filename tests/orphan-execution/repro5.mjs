// Repro 5: padrão exato do svc-api — sessão RedWire longeva (como
// reddb-runtime), KV PUT com TAGS, payloads de tamanho real do worker
// (~300 bytes), sessão com transação pendente e multiplex, e o próprio
// padrão de gravação do park-usage-export (worker loop com receipt).
// O wedge do stg aconteceu nesse caminho com o holder preso 31 min.
import { connectRedwire } from '../../drivers/js-client/src/redwire.js'

const host = process.argv[2] ?? '127.0.0.1'
const port = Number(process.argv[3] ?? 15050)
const durSecs = Number(process.argv[4] ?? 300)
const ENV = 'stg'
const POD = 'api-queue-worker-795674fb7b'
const NAME = 'organization-usage-delivery'
const KEY = `${ENV}:${POD}:${NAME}`

const sleep = ms => new Promise(r => setTimeout(r, ms))

// Worker do svc-api: sessão longeva, receipt a cada execução.
async function svcWorker(id, stats, deadline) {
  let client
  try { client = await connectRedwire({ host, port, clientName: `svc${id}` }) }
  catch { return }
  try {
    await client.call('query', { sql: `BEGIN` })
    let run = 0
    while (Date.now() < deadline) {
      run++
      const receipt = JSON.stringify({
        worker: NAME, environment: ENV, pod: `${POD}-${id}`, outcome: 'succeeded',
        startedAt: new Date().toISOString(), completedAt: new Date().toISOString(),
        durationMs: 40 + run, seq: run,
      })
      const sql = `KV PUT '${KEY}-${id}' = '${receipt}'`
      const t0 = Date.now()
      await client.call('query', { sql })          // PUT com transação pendente
      stats.putMs.push(Date.now() - t0); stats.puts++
      // desconecta abruptamente de vez em quando com a transação ABERTA
      // (pod morto em OOMKilled/evicted) — como no incidente.
      if (run % 25 === 0) {
        stats.sessionDrops++
        client.socket.destroy()
        client = await connectRedwire({ host, port, clientName: `svc${id}` })
        await client.call('query', { sql: `BEGIN` })
      }
    }
  } catch (e) {
    stats.errors++; try { client?.socket.destroy() } catch {}
  }
}

// Concorrência de leitura (o app faz listing enquanto o worker escreve)
async function readerLoop(stats, deadline) {
  let client
  try { client = await connectRedwire({ host, port, clientName: 'reader' }) }
  catch { return }
  try {
    while (Date.now() < deadline) {
      const t0 = Date.now()
      await client.call('query', { sql: `KV GET '${KEY}-0'` })
      stats.getMs.push(Date.now() - t0); stats.gets++
      await sleep(30)
    }
  } catch { stats.errors++ }
}

const stats = { puts: 0, putMs: [], gets: 0, getMs: [], sessionDrops: 0, errors: 0 }
const deadline = Date.now() + durSecs * 1000
console.log(`repro5: 3 workers svc-style + reader por ${durSecs}s`)
await Promise.allSettled([
  svcWorker(1, stats, deadline), svcWorker(2, stats, deadline), svcWorker(3, stats, deadline),
  readerLoop(stats, deadline),
])
const sorted = [...stats.putMs].sort((a, b) => a - b)
const p50 = sorted[Math.floor(sorted.length / 2)] ?? -1
const p99 = sorted[Math.floor(sorted.length * 0.99)] ?? -1
console.log(`final: puts=${stats.puts} gets=${stats.gets} sessionDrops=${stats.sessionDrops} err=${stats.errors}`)
console.log(`putP50=${p50}ms putP99=${p99}ms`)
