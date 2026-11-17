// Repro 4: transação órfã — sessão faz BEGIN, PUT e morre SEM COMMIT.
// Depois: outras sessões tentam escrever no MESMO key e medimos a latência.
// Se a teardown do disconnect não rolar back a transação órfã, os writes
// seguintes no mesmo key bloqueiam para sempre (wedge).
import { connectRedwire } from '../../drivers/js-client/src/redwire.js'

const host = process.argv[2] ?? '127.0.0.1'
const port = Number(process.argv[3] ?? 15050)
const KEY = 'orphan:tx:key1'
const sleep = ms => new Promise(r => setTimeout(r, ms))

async function q(client, sql) { return await client.call('query', { sql }) }

// 1. Sessão A: BEGIN + PUT + morte abrupta (sem COMMIT)
console.log('--- fase 1: criar transação órfã ---')
const a = await connectRedwire({ host, port, clientName: 'orphanA' })
console.log('BEGIN:', JSON.stringify(await q(a, 'BEGIN')).slice(0, 80))
console.log('PUT  :', JSON.stringify(await q(a, `KV PUT '${KEY}' = '{"v":1}'`)).slice(0, 80))
a.socket.destroy()  // morre com transação aberta
console.log('sessão A destruída com transação aberta')
await sleep(1000)

// 2. Sessão B: escreve no MESMO key, com timeout de 10s por tentativa
console.log('--- fase 2: writes de outra sessão no mesmo key ---')
const b = await connectRedwire({ host, port, clientName: 'victimB' })
for (let i = 0; i < 5; i++) {
  const t0 = Date.now()
  try {
    const p = Promise.race([
      q(b, `KV PUT '${KEY}' = '{"v":${i + 2}}'`),
      sleep(10000).then(() => { throw new Error('TIMEOUT_10S') }),
    ])
    const r = await p
    console.log(`write ${i}: OK em ${Date.now() - t0}ms`)
  } catch (e) {
    console.log(`write ${i}: FALHOU após ${Date.now() - t0}ms — ${String(e?.message ?? e).slice(0, 80)}`)
    if (String(e).includes('TIMEOUT_10S')) { console.log('*** BLOQUEIO: write pendurado >10s'); break }
  }
}

// 3. Session C: BEGIN + PUT em outra coleção + sessão B escreve no key da C
console.log('--- fase 3: transação viva A/B ---')
const c = await connectRedwire({ host, port, clientName: 'txC' })
await q(c, 'BEGIN').catch(e => console.log('BEGIN C err:', e.message?.slice(0, 60)))
console.log('C BEGIN ok')
const d = await connectRedwire({ host, port, clientName: 'victimD' })
const t0 = Date.now()
try {
  const p = Promise.race([
    q(d, `KV PUT 'orphan:tx:key2' = '{"v":9}'`),
    sleep(8000).then(() => { throw new Error('TIMEOUT_8S') }),
  ])
  await p
  console.log(`D escreve key2 (fora da tx de C): OK em ${Date.now() - t0}ms`)
} catch (e) { console.log(`D bloqueado: ${String(e).slice(0, 60)}`) }
c.socket.destroy()
d.socket.destroy()
b.socket.destroy()
console.log('fim repro4')
