import { countFrameCopies, openFrameReader } from '../test/fixtures/frame-reader.mjs'

// Production reader and decoder, with scripted transport events and prebuilt input.
export async function measureFrames(baseline, candidate, { rounds = 20, sampleMs = 20 } = {}) {
  const workloads = [
    { name: 'contiguous-1KiB', size: 1024, chunkSize: 1024, count: 1 },
    { name: 'contiguous-1MiB', size: 1024 * 1024, chunkSize: 1024 * 1024, count: 1 },
    { name: 'fragmented-1MiB-64KiB', size: 1024 * 1024, chunkSize: 65536, count: 1 },
    { name: 'fragmented-1MiB-1KiB', size: 1024 * 1024, chunkSize: 1024, count: 1 },
    { name: 'coalesced-128x1KiB', size: 1024, chunkSize: 128 * 1024, count: 128 },
    { name: 'bytewise-64B', size: 64, chunkSize: 1, count: 1 },
  ]
  let sink
  const sample = async (run) => {
    let count = 0
    const start = performance.now()
    let elapsed
    do {
      sink = await run()
      count++
      elapsed = performance.now() - start
    } while (elapsed < sampleMs)
    return elapsed * 1000 / count
  }
  const results = []
  for (const workload of workloads) {
    const { name, size, count, chunkSize } = workload
    const input = new Uint8Array(size * count)
    const expected = []
    for (let index = 0; index < count; index++) {
      const bytes = baseline.encodeFrame(baseline.MessageKind.Result, BigInt(index + 1), new Uint8Array(size - 16).fill(index + 1))
      input.set(bytes, size * index)
      expected.push(baseline.decodeFrame(bytes))
    }
    const chunks = []
    for (let offset = 0; offset < input.length; offset += chunkSize) chunks.push(input.subarray(offset, offset + chunkSize))
    const functions = {}
    const accumulatorCopiedBytes = {}
    for (const [variant, core] of Object.entries({ baseline, javascript: candidate })) {
      const { socket, reader } = await openFrameReader(core)
      const run = () => {
        const pending = expected.map(() => reader.next())
        for (const chunk of chunks) socket.emit('data', chunk)
        return Promise.all(pending)
      }
      functions[variant] = run
      let pending
      accumulatorCopiedBytes[variant] = countFrameCopies(() => { pending = run() })
      const frames = await pending
      for (let index = 0; index < expected.length; index++) {
        const actual = frames[index]
        const original = expected[index]
        if (actual.kind !== original.kind || actual.flags !== original.flags || actual.streamId !== original.streamId
          || actual.correlationId !== original.correlationId || actual.consumed !== original.consumed
          || actual.payload.length !== original.payload.length || actual.payload.some((byte, offset) => byte !== original.payload[offset])) {
          throw new Error(`${name}/${variant}: decoded frame differs`)
        }
      }
      await sample(run)
    }
    const samples = { baseline: [], javascript: [] }
    for (let round = 0; round < rounds; round++) {
      for (const variant of (round % 2 ? ['javascript', 'baseline'] : ['baseline', 'javascript'])) {
        samples[variant].push(await sample(functions[variant]))
      }
    }
    results.push({ ...workload, inputBytes: input.length, accumulatorCopiedBytes, samples })
  }
  return { rounds, sampleMs, results, sinkPayloadBytes: sink.reduce((sum, frame) => sum + frame.payload.length, 0) }
}
