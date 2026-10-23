import { readFileSync } from 'node:fs'

const report = JSON.parse(readFileSync(process.argv[2], 'utf8'))
const median = (values) => {
  const sorted = [...values].sort((a, b) => a - b)
  const middle = Math.floor(sorted.length / 2)
  return sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2
}
let seed = 1729
const random = () => {
  seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0
  return seed / 2 ** 32
}
function speedup(baseline, candidate) {
  const ratios = []
  for (let round = 0; round < 2000; round++) {
    const indices = baseline.map(() => Math.floor(random() * baseline.length))
    ratios.push(median(indices.map((i) => baseline[i])) / median(indices.map((i) => candidate[i])))
  }
  ratios.sort((a, b) => a - b)
  return `${(median(baseline) / median(candidate)).toFixed(2)}× [${ratios[50].toFixed(2)}, ${ratios[1949].toFixed(2)}]`
}

console.log('| Runtime | Input | Original µs | JS µs | WASM µs | JS speedup [95% CI] | WASM speedup [95% CI] |')
console.log('| --- | --- | ---: | ---: | ---: | ---: | ---: |')
for (const runtime of ['node', 'chromium', 'firefox', 'webkit']) {
  const result = report[runtime]
  if (!result) continue
  for (const { name, samples } of result.results) {
    console.log(`| ${runtime} | ${name} | ${median(samples.baseline).toFixed(2)} | ${median(samples.javascript).toFixed(2)} | ${median(samples.wasm).toFixed(2)} | ${speedup(samples.baseline, samples.javascript)} | ${speedup(samples.baseline, samples.wasm)} |`)
  }
}
