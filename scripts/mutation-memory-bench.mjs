// Diagnostic before/after SDK timings, not a cross-database benchmark.
// memory:// uses an ephemeral persistent file, including the embedded WAL.
// Build both binaries with the same profile. Run with no concurrent builds:
// REDDB_MEMORY_BUDGET=67108864 node scripts/mutation-memory-bench.mjs BEFORE AFTER
import { connect } from '../drivers/js/src/index.js';
import { performance } from 'node:perf_hooks';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';

const [before, after] = process.argv.slice(2);
if (!before || !after) throw new Error('Usage: node scripts/mutation-memory-bench.mjs BEFORE AFTER');
const binaries = { before, after };
const samples = { single: { before: [], after: [] }, bulk: { before: [], after: [] } };
const statementSamples = { single: { before: [], after: [] }, bulk: { before: [], after: [] } };
for (const mode of ['single', 'bulk']) {
  for (let run = 0; run < 7; run++) {
    for (const label of run % 2 ? ['after', 'before'] : ['before', 'after']) {
      const db = await connect('memory://', { binary: binaries[label] });
      try {
        await db.query('CREATE TABLE perf_growth (id INT, payload TEXT)');
        const rows = mode === 'single' ? 1 : 2500;
        for (let offset = 0; offset < rows; offset += 500) {
          const values = Array.from({ length: Math.min(500, rows - offset) }, (_, i) => `(${offset + i},'seed')`).join(',');
          await db.query(`INSERT INTO perf_growth (id,payload) VALUES ${values}`);
        }
        const query = 'UPDATE perf_growth SET payload = $1' + (mode === 'single' ? ' WHERE id = 0' : '');
        for (let i = 0; i < (mode === 'single' ? 30 : 1); i++) await db.query(query, ['warmup']);
        const iterations = mode === 'single' ? 300 : 8;
        const statements = [];
        const start = performance.now();
        for (let i = 0; i < iterations; i++) {
          const statementStart = performance.now();
          await db.query(query, [`value-${i}`]);
          statements.push(performance.now() - statementStart);
        }
        const elapsed = (performance.now() - start) / iterations;
        const readback = await db.query('SELECT id,payload FROM perf_growth');
        const ids = new Set();
        for (const row of readback.rows) {
          if (!Number.isInteger(row.id) || row.id < 0 || row.id >= rows || ids.has(row.id)
              || row.payload !== `value-${iterations - 1}`) {
            throw new Error(`${mode}/${label}: unexpected key, duplicate key or payload`);
          }
          ids.add(row.id);
        }
        if (ids.size !== rows) throw new Error(`${mode}/${label}: missing keys`);
        samples[mode][label].push(elapsed);
        statementSamples[mode][label].push(statements);
        console.error(`${mode}/${label} run ${run + 1}/7: ${elapsed.toFixed(3)} ms/statement`);
      } finally {
        await db.close();
      }
    }
  }
}
const result = {
  node: process.version,
  classification: 'diagnostic; host exclusivity and build equivalence must be verified separately',
  storage: 'memory:// (ephemeral file with embedded WAL, not a RAM-only backend)',
  workload: { runs: 7, single: { rows: 1, warmups: 30, iterations: 300 }, bulk: { rows: 2500, warmups: 1, iterations: 8 } },
  budget: process.env.REDDB_MEMORY_BUDGET ?? 'host-detected',
  binary_sha256: Object.fromEntries(Object.entries(binaries).map(([label, path]) => [label, createHash('sha256').update(readFileSync(path)).digest('hex')])),
  // Independent process runs remain the unit of comparison; statements within
  // one run are correlated and must not be treated as independent repetitions.
  samples_ms: samples,
  statement_samples_ms: statementSamples,
  statement_percentiles_ms: Object.fromEntries(Object.entries(statementSamples).map(([mode, labels]) =>
    [mode, Object.fromEntries(Object.entries(labels).map(([label, runs]) => [label, runs.map(values => {
      const sorted = [...values].sort((a, b) => a - b);
      const percentile = fraction => sorted[Math.ceil(fraction * sorted.length) - 1];
      return { p50: percentile(0.5), p95: percentile(0.95), p99: percentile(0.99) };
    })]))])),
  summary: Object.fromEntries(Object.entries(samples).map(([mode, values]) => {
    const median = label => [...values[label]].sort((a, b) => a - b)[3];
    return [mode, { before_ms: median('before'), after_ms: median('after'), change_percent: (median('after') / median('before') - 1) * 100 }];
  })),
};
console.log(JSON.stringify(result, null, 2));
