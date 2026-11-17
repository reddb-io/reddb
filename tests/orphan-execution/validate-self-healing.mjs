// Validação do self-healing (fix do watchdog compartilhado).
// Simula um wedge de RedWire: saturar o admission e pendurar uma execução
// além do deadline. Esperado na imagem com o fix:
//   1. /health/ready responde 200 enquanto tudo está saudável;
//   2. se uma execução passa do deadline e permanece inflight,
//      /health/ready passa a 503 com reason handler_deadline_exceeded;
//   3. o K8s (readinessProbe) reinicia o pod — self-healing.
// Uso: node validate-self-healing.mjs <host> <wirePort> <httpPort>

import { connectRedwire } from '../../drivers/js-client/src/redwire.js'

const host = process.argv[2] ?? '127.0.0.1'
const wirePort = Number(process.argv[3] ?? 15050)
const httpPort = Number(process.argv[4] ?? 15000)
const httpBase = `http://${host}:${httpPort}`

const sleep = ms => new Promise(r => setTimeout(r, ms))

async function readyState() {
  const res = await fetch(`${httpBase}/health/ready`, { signal: AbortSignal.timeout(5000) })
  const body = await res.json().catch(() => ({}))
  return { status: res.status, ready: body.status, reason: body.reason }
}

// --- passo 1: baseline saudável ---
const before = await readyState()
console.log(`antes:      HTTP ${before.status} status=${before.ready}`)
if (before.status !== 200) {
  console.log('FALHA: esperava 200 no baseline saudável')
  process.exit(1)
}

// --- passo 2: saturar o admission e pendurar execuções ---
// active_max=2 (2 CPUs). Abro 8 conexões e disparo PUTs sem consumir as
// respostas; em seguida destruo as sessões no meio do processamento.
// Com o fix, qualquer execução que ultrapassar o deadline marca o watchdog.
const clients = []
for (let i = 0; i < 8; i++) {
  try {
    const c = await connectRedwire({ host, port: wirePort, clientName: `sh${i}` })
    clients.push(c)
  } catch { /* fila cheia é aceitável */ }
}
console.log(`sessões abertas: ${clients.length}`)

// Dispara PUTs longos (payload grande) sem aguardar resposta e destrói os sockets:
for (const c of clients) {
  for (let k = 0; k < 4; k++) {
    const payload = 'x'.repeat(64 * 1024)
    c.call('query', { sql: `KV PUT 'sh:wedge:${k}' = '${payload}'` }).catch(() => {})
  }
}
// Destrói tudo no meio: o servidor executa, mas a resposta não tem destino.
for (const c of clients) { try { c.socket.destroy() } catch {} }
console.log('sessões destruídas no meio das execuções; aguardando deadline…')

// --- passo 3: observa o readiness por até 60s (deadline é 30s) ---
const deadlineAt = Date.now() + 60_000
let degraded = false
while (Date.now() < deadlineAt) {
  await sleep(3000)
  const now = await readyState().catch(() => ({ status: 'ERR', ready: '?' }))
  console.log(`  probe: HTTP ${now.status} status=${now.ready} reason=${now.reason ?? '-'}`)
  if (now.status === 503) { degraded = true; break }
}

console.log('--- resultado ---')
if (degraded) {
  console.log('✅ SELF-HEALING VALIDADO: wedge degrada /health/ready para 503 (o K8s reiniciaria o pod).')
  process.exit(0)
}
console.log('❌ NÃO DEGRADOU: o wedge continua invisível ao readiness.')
process.exit(2)
