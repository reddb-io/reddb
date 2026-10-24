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

const columns = ['Runtime', 'Input', 'Original µs', 'JS µs']
if (report.wasm) columns.push('WASM µs')
columns.push('JS speedup [95% CI]')
if (report.wasm) columns.push('WASM speedup [95% CI]')
console.log(`| ${columns.join(' | ')} |`)
console.log(`| ${columns.map((_, index) => index < 2 ? '---' : '---:').join(' | ')} |`)
for (const runtime of ['node', 'bun', 'chromium', 'firefox', 'webkit']) {
  const result = report[runtime]
  if (!result) continue
  for (const { name, samples } of result.results) {
    const row = [runtime, name, median(samples.baseline).toFixed(2), median(samples.javascript).toFixed(2)]
    if (report.wasm) row.push(median(samples.wasm).toFixed(2))
    row.push(speedup(samples.baseline, samples.javascript))
    if (report.wasm) row.push(speedup(samples.baseline, samples.wasm))
    console.log(`| ${row.join(' | ')} |`)
  }
}
