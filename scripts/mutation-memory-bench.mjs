// Diagnostic before/after SDK timings, not a cross-database benchmark.
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
        const start = performance.now();
        for (let i = 0; i < iterations; i++) await db.query(query, [`value-${i}`]);
        const elapsed = (performance.now() - start) / iterations;
        const readback = await db.query('SELECT id FROM perf_growth WHERE payload = $1', [`value-${iterations - 1}`]);
        if (readback.rows.length !== rows) throw new Error(`${mode}/${label}: readback mismatch`);
        samples[mode][label].push(elapsed);
      } finally {
        await db.close();
      }
    }
  }
}
const result = {
  node: process.version,
  budget: process.env.REDDB_MEMORY_BUDGET ?? 'host-detected',
  binary_sha256: Object.fromEntries(Object.entries(binaries).map(([label, path]) => [label, createHash('sha256').update(readFileSync(path)).digest('hex')])),
  samples_ms: samples,
  summary: Object.fromEntries(Object.entries(samples).map(([mode, values]) => {
    const median = label => [...values[label]].sort((a, b) => a - b)[3];
    return [mode, { before_ms: median('before'), after_ms: median('after'), change_percent: (median('after') / median('before') - 1) * 100 }];
  })),
};
console.log(JSON.stringify(result, null, 2));
