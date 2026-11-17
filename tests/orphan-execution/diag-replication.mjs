// Diagnóstico de replicação: lê os marcadores nos dois lados e compara.
import { connectRedwire } from '../../drivers/js-client/src/redwire.js'

const read = async (port, label, keys) => {
  const c = await connectRedwire({ host: '127.0.0.1', port, clientName: `diag-${label}` })
  for (const key of keys) {
    const r = await c.call('query', { sql: `KV GET '${key}'` })
    console.log(`${label} ${key} →`, r.rows?.[0]?.value?.slice(0, 80) ?? '(não encontrado)')
  }
  c.socket.end()
}

const keys = ['repl:phase3', 'repl:test']
await read(5050, 'primary', keys)
await read(5051, 'replica', keys)
