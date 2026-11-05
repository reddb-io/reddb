import { arch, cpus, platform } from 'node:os'
import { resolve } from 'node:path'
import { pathToFileURL } from 'node:url'
import { connection, countReceiveCopies, frame } from './test/frame-fixture'
import { RedDBConnection } from './index'

const root = resolve(import.meta.dir, '../..')
const baselineRoot = resolve(process.env.REDDB_BENCH_BASELINE || root)
const baseline = await import(pathToFileURL(resolve(baselineRoot, 'drivers/bun/index.ts')).href)
const rounds = Number(process.env.REDDB_BENCH_ROUNDS || 20)
const sampleMs = Number(process.env.REDDB_BENCH_SAMPLE_MS || 20)
const revision = (cwd: string) => {
  const result = Bun.spawnSync(['git', 'rev-parse', 'HEAD'], { cwd })
  if (result.exitCode !== 0) throw new Error(result.stderr.toString())
  return result.stdout.toString().trim()
}
const workloads = [
  { name: 'contiguous-1KiB', size: 1024, chunkSize: 1024, count: 1 },
  { name: 'contiguous-1MiB', size: 1024 * 1024, chunkSize: 1024 * 1024, count: 1 },
  { name: 'fragmented-1MiB-64KiB', size: 1024 * 1024, chunkSize: 65536, count: 1 },
  { name: 'fragmented-1MiB-1KiB', size: 1024 * 1024, chunkSize: 1024, count: 1 },
  { name: 'coalesced-128x1KiB', size: 1024, chunkSize: 128 * 1024, count: 128 },
  { name: 'bytewise-64B', size: 64, chunkSize: 1, count: 1 },
]
let sink: string[] = []
const sample = async (run: () => Promise<string[]>) => {
  const start = performance.now()
  let count = 0
  let elapsed: number
  do {
    sink = await run()
    count++
    elapsed = performance.now() - start
  } while (elapsed < sampleMs)
  return elapsed * 1000 / count
}
const results = []
for (const workload of workloads) {
  const { size, count, chunkSize } = workload
  const expected = Array.from({ length: count }, (_, index) => String.fromCharCode(97 + index % 26).repeat(size - 16))
  const input = Buffer.concat(expected.map(value => frame(value)))
  const chunks: Buffer[] = []
  for (let offset = 0; offset < input.length; offset += chunkSize) chunks.push(input.subarray(offset, offset + chunkSize))
  const functions: Record<string, () => Promise<string[]>> = {}
  const receiveCopies: Record<string, ReturnType<typeof countReceiveCopies>> = {}
  for (const [variant, Constructor] of Object.entries({ baseline: baseline.RedDBConnection, javascript: RedDBConnection })) {
    const { db, socket } = connection(Constructor)
    // Keep sent frames out of both memory pressure and timings; this socket performs no I/O.
    socket.write = bytes => bytes.length
    const start = () => expected.map(() => db.query('SELECT probe'))
    const deliver = () => { for (const chunk of chunks) db._onData(chunk) }
    functions[variant] = () => { const pending = start(); deliver(); return Promise.all(pending) }
    const pending = start()
    receiveCopies[variant] = countReceiveCopies(deliver)
    const actual = await Promise.all(pending)
    if (actual.some((value, index) => value !== expected[index])) throw new Error(`${workload.name}/${variant}: response bytes differ`)
    await sample(functions[variant])
  }
  const samples: Record<string, number[]> = { baseline: [], javascript: [] }
  for (let round = 0; round < rounds; round++) {
    for (const variant of (round % 2 ? ['javascript', 'baseline'] : ['baseline', 'javascript'])) {
      samples[variant].push(await sample(functions[variant]))
    }
  }
  results.push({ ...workload, inputBytes: input.length, receiveCopies, samples })
}
const report = {
  capturedAt: new Date().toISOString(),
  host: { platform: platform(), arch: arch(), cpu: cpus()[0]?.model },
  head: revision(root), baseline: revision(baselineRoot),
  method: 'Bun production query() on scripted sockets: request encoding, receive parsing, owned payload, UTF-8 conversion and Promise delivery. Prebuilt input and byte-equivalence checks are outside timing. Copy probes run separately and count top-level Buffer.concat/from/copy bytes without double counting. The unchanged Bun TCP callback ownership copy is outside these scripted events. No network, engine execution or total process memory is measured.',
  bun: { version: Bun.version, rounds, sampleMs, results, sinkBytes: sink.reduce((sum, value) => sum + value.length, 0) },
}
if (process.env.REDDB_BENCH_OUTPUT) await Bun.write(process.env.REDDB_BENCH_OUTPUT, JSON.stringify(report, null, 2) + '\n')
console.log(JSON.stringify(report, null, 2))
