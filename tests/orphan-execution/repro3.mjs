// Repro 3: disconnect no MEIO do processamento de um PUT.
// O cliente envia o frame de query e destrói o socket imediatamente depois,
// enquanto o engine processa. Simultaneamente, N-1 conexões fazem PUTs
// contínuos para ocupar os slots de execução.
import { connectRedwire } from '../../drivers/js-client/src/redwire.js'

const host = process.argv[2] ?? '127.0.0.1'
const port = Number(process.argv[3] ?? 15050)
const durSecs = Number(process.argv[4] ?? 300)
const KEY = 'stg:api-queue-worker:organization-usage-delivery'

const sleep = ms => new Promise(r => setTimeout(r, ms))

async function killerLoop(stats) {
  // Envia um PUT e destrói o socket ANTES de ler a resposta —
  // o engine executa o handler num worker do spawn_blocking e a
  // resposta vai para um socket morto.
  while (stats.kills < 100000) {
    let client
    try { client = await connectRedwire({ host, port, clientName: 'killer' }) }
    catch { stats.connectFails++; await sleep(50); continue }
    try {
      const payload = JSON.stringify({ outcome: 'failed', error: 'RedDBError', seq: stats.kills })
      const sql = `KV PUT '${KEY}:kill' = '${payload}'`
      client.socket.write('') // flush noop
      client.call('query', { sql }).catch(() => {})   // dispara, não espera
      await sleep(1)                                   // dá 1ms para o frame chegar e o servidor despachar
      client.socket.destroy()                          // mata o socket no meio do processamento
      stats.kills++
    } catch { stats.kills++ }
    await sleep(20)
  }
}

async function fillerLoop(id, stats, deadline) {
  let client
  try { client = await connectRedwire({ host, port, clientName: `filler${id}` }) }
  catch { return }
  try {
    while (Date.now() < deadline) {
      const payload = JSON.stringify({ outcome: 'ok', filler: id, ts: Date.now() })
      const t0 = Date.now()
      await client.call('query', { sql: `KV PUT '${KEY}:fill${id}' = '${payload}'` })
      stats.putMs.push(Date.now() - t0); stats.puts++
    }
    client.socket.end()
  } catch (e) { stats.errors++; try { client.socket.destroy() } catch {} }
}

const stats = { kills: 0, puts: 0, putMs: [], errors: 0, connectFails: 0 }
const deadline = Date.now() + durSecs * 1000
console.log(`repro3: killers + 1 filler por ${durSecs}s`)

// 1 filler ocupando 1 slot; killers consomem o resto. Com bulk=1 e active=2,
// basta 1 handler preso no outro slot para a fila saturar.
const jobs = [fillerLoop(1, stats, deadline), killerLoop(stats)]
await Promise.allSettled(jobs)

console.log(`final: kills=${stats.kills} puts=${stats.puts} err=${stats.errors}`)
