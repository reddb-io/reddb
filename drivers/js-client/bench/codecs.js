// Transport-free encoding measurements: normalization, parameters and frame.
// Input/output copies performed by wasm-bindgen are inside the measured call.
export async function measureCodecs(baseline, candidate, wasm, options = {}) {
  const rounds = options.rounds ?? 20
  const sampleMs = options.sampleMs ?? 20
  const sql = 'SELECT * FROM vectors WHERE embedding = $1'
  const workloads = [
    ['vector-8', new Float32Array(8).fill(0.25)],
    ['vector-1536', Float32Array.from({ length: 1536 }, (_, i) => (i % 31) / 32)],
    ['vector-65536', new Float32Array(65536).fill(0.25)],
    ['bytes-1024', new Uint8Array(1024).fill(127)],
    ['bytes-262144', new Uint8Array(262144).fill(127)],
  ]
  const encode = (module, value, binary) => module.encodeFrame(
    module.MessageKind.QueryWithParams, 1n,
    module.encodeQueryWithParams(sql, module.normalizeQueryParams([[value]], binary)),
  )
  let sink
  const sample = (run) => {
    let count = 0
    const start = performance.now()
    let elapsed
    do {
      for (let i = 0; i < 16; i++) sink = run()
      count += 16
      elapsed = performance.now() - start
    } while (elapsed < sampleMs)
    return elapsed * 1000 / count
  }
  const results = []
  for (const [name, value] of workloads) {
    const functions = {
      baseline: () => encode(baseline, value, false),
      javascript: () => encode(candidate, value, true),
      wasm: () => {
        const [param] = candidate.normalizeQueryParams([[value]], true)
        return value instanceof Float32Array
          ? wasm.encodeVectorQueryFrame(sql, param)
          : wasm.encodeBytesQueryFrame(sql, param)
      },
    }
    const expected = functions.baseline()
    for (const [variant, run] of Object.entries(functions)) {
      const bytes = run()
      if (bytes.length !== expected.length || bytes.some((byte, i) => byte !== expected[i])) {
        throw new Error(`${name}/${variant}: wire bytes differ from the original JS client`)
      }
      sample(run) // warm each complete pipeline before measurement
    }
    const samples = { baseline: [], javascript: [], wasm: [] }
    for (let round = 0; round < rounds; round++) {
      // Rotate order to avoid consistently giving one implementation a GC/JIT advantage.
      const names = Object.keys(functions)
      for (let i = 0; i < names.length; i++) {
        const variant = names[(round + i) % names.length]
        samples[variant].push(sample(functions[variant]))
      }
    }
    results.push({ name, frameBytes: expected.length, samples })
  }
  return { rounds, sampleMs, results, sinkBytes: sink.length }
}
