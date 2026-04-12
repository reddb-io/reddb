#!/usr/bin/env node
/**
 * RedDB Node.js Wire Protocol Benchmark
 * Usage: node bench.js [host:port]
 */
const { connect } = require('./index')

const ADDR = process.argv[2] || '127.0.0.1:5050'
const N = 5000
const POINT_LOOKUPS = 200
const RANGE_QUERIES = 50
const FILTER_QUERIES = 50

function rng(seed) {
  let s = seed
  return () => { s = (s * 1103515245 + 12345) & 0x7fffffff; return s }
}

async function main() {
  const rand = rng(42)
  const conn = await connect(ADDR)
  console.log(`Connected to ${ADDR}`)

  // Insert
  const records = []
  const cities = ['NYC', 'London', 'Tokyo', 'Paris', 'Berlin']
  for (let i = 0; i < N; i++) {
    records.push(JSON.stringify({
      fields: { id: i + 1, name: `User_${i}`, age: 18 + (rand() % 63), city: cities[rand() % 5], score: (rand() % 10000) / 100 }
    }))
  }

  let t0 = performance.now()
  const count = await conn.bulkInsert('users', records)
  let ms = performance.now() - t0
  console.log(`  insert_bulk:     ${(N / ms * 1000).toFixed(0).padStart(8)} ops/sec (${ms.toFixed(0)}ms) [${count} rows]`)

  // Point lookups
  const lids = Array.from({ length: POINT_LOOKUPS }, () => 1 + (rand() % N))
  t0 = performance.now()
  for (const rid of lids) {
    await conn.queryRaw(`SELECT * FROM users WHERE _entity_id = ${rid}`)
  }
  ms = performance.now() - t0
  console.log(`  select_point:    ${(POINT_LOOKUPS / ms * 1000).toFixed(0).padStart(8)} ops/sec (${ms.toFixed(0)}ms)`)

  // Range queries
  const rqs = Array.from({ length: RANGE_QUERIES }, () => {
    const l = 18 + (rand() % 53); return [l, l + 10]
  })
  t0 = performance.now()
  for (const [l, h] of rqs) {
    await conn.queryRaw(`SELECT * FROM users WHERE age BETWEEN ${l} AND ${h}`)
  }
  ms = performance.now() - t0
  console.log(`  select_range:    ${(RANGE_QUERIES / ms * 1000).toFixed(0).padStart(8)} ops/sec (${ms.toFixed(0)}ms)`)

  // Filtered queries
  const fqs = Array.from({ length: FILTER_QUERIES }, () => [cities[rand() % 5], 18 + (rand() % 43)])
  t0 = performance.now()
  for (const [c, a] of fqs) {
    await conn.queryRaw(`SELECT * FROM users WHERE city = '${c}' AND age > ${a}`)
  }
  ms = performance.now() - t0
  console.log(`  select_filtered: ${(FILTER_QUERIES / ms * 1000).toFixed(0).padStart(8)} ops/sec (${ms.toFixed(0)}ms)`)

  conn.close()
}

main().catch(console.error)
