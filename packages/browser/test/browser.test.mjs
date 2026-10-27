import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { once } from 'node:events'
import { createServer } from 'node:http'
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, extname, resolve, sep } from 'node:path'
import { fileURLToPath } from 'node:url'
import { after, before, describe, test } from 'node:test'
import { chromium, firefox, webkit } from 'playwright'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..')
const toolchain = (await readFile(resolve(root, 'rust-toolchain.toml'), 'utf8')).match(/channel\s*=\s*"([^"]+)"/)[1]
const browserTypes = { chromium, firefox, webkit }
const browsers = (process.env.REDDB_BROWSER_BROWSERS || 'chromium,firefox,webkit').split(',')
let server
let origin
let fixtures
let nativeTreeFixture
let nativeFixture
let nativeSnapshot
let nativeQueryFixture
const networkRequests = []

async function withHostOffline(run) {
  // Also disconnect the HTTP listener: startup and recovery must succeed
  // without a reachable host, including across a complete browser restart.
  const { port } = server.address()
  await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()))
  try {
    await run()
  } finally {
    server.listen(port, '127.0.0.1')
    await once(server, 'listening')
  }
}

function nativeArtifact(action, path) {
  execFileSync('cargo', [`+${toolchain}`, 'run', '--quiet', '--locked', '-p', 'reddb-io-file', '--example', 'browser_artifact', '--', action, path], { cwd: root, stdio: 'inherit' })
}

function nativeTree(action, path) {
  execFileSync('cargo', [`+${toolchain}`, 'run', '--quiet', '--locked', '-p', 'reddb-io-engine', '--example', 'browser_btree', '--', action, path], { cwd: root, stdio: 'inherit' })
}

async function sharedConformance(browserPage) {
  const passed = await browserPage.evaluate(async () => {
    const worker = new Worker('/packages/browser/test/query-worker.js', { type: 'module' })
    try {
      return await new Promise((resolve, reject) => {
        worker.onmessage = ({ data }) => data.error ? reject(new Error(data.error)) : resolve(data.passed)
        worker.onerror = (event) => reject(new Error(event.message))
      })
    } finally { worker.terminate() }
  })
  assert.deepEqual(passed, [
    'scalar_goldens', 'scalar_failures', 'row_execution', 'binary_document_evaluation', 'join_execution',
    'aggregation_execution', 'window_execution', 'set_execution', 'cancellation',
    'page_layout_and_corruption', 'freelist_trunk_recovery', 'cache_eviction_and_writeback',
    'btree_persisted_round_trip', 'btree_reclamation_and_cursor_lifetime', 'btree_bounds_and_corruption',
    'pager_orchestration_restart', 'pager_orchestration_allocation_fence', 'pager_orchestration_wal_barrier',
    'pager_orchestration_checkpoint_recovery', 'pager_orchestration_open_policy',
    'pager_barriers_and_restart', 'pager_failure_boundaries',
    'pager_multiframe_checkpoint_io_and_crash_recovery',
    'pager_frame_bounds_and_read_errors', 'pager_superblock_failure_boundaries',
    'pager_recovery_failure_boundaries',
    'mvcc_snapshot_isolation_and_read_committed', 'mvcc_failed_prepare_aborts_all_writers',
    'mvcc_publication_guard_covers_visibility', 'mvcc_savepoints_and_connection_isolation',
    'mvcc_retention_and_checkpoint_outcomes', 'mvcc_unique_keys_follow_outcomes',
    'mvcc_serializable_write_skew', 'mvcc_serializable_vault_predicates',
    'mvcc_autocommit_reservations_and_replay',
    'entity_models_and_metadata', 'entity_document_field_bloom', 'entity_auxiliary_and_timestamps',
    'entity_own_subtransaction_visibility', 'entity_logical_version_selection',
    'entity_serializable_logical_identity',
    'entity_codec_native_v1_v12_goldens', 'entity_codec_value_tags_and_truncation',
    'entity_codec_invalid_lengths_and_depth', 'entity_codec_optional_tails_and_truncation',
    'record_native_materialization_goldens', 'record_schema_and_graph_representation',
    'record_projection_and_document_body', 'record_typed_timeseries_and_identity',
    'segment_native_entity_shapes', 'segment_key_versions_and_mutation',
    'segment_bounded_scan_and_mutation', 'segment_metadata_and_sealing',
    'metadata_codec_native_goldens', 'metadata_codec_record_envelopes',
    'metadata_codec_malformed_payloads', 'metadata_codec_depth_boundary',
    'snapshot_native_store_fixture', 'snapshot_native_v1_v12_recovery', 'snapshot_empty_collections_and_legacy_tails',
    'snapshot_corruption_and_callback_errors', 'snapshot_resident_recovery_options', 'snapshot_native_committed_projection',
    'manager_models_and_constraint_lock', 'manager_snapshot_key_versions',
    'manager_moderation_and_serializable_reads', 'manager_bounded_scan_and_consolidation',
    'manager_ordered_graph_candidates',
    'manager_restored_identity_keeps_cursor_boundary',
    'resident_index_document_tokens', 'resident_index_replacement_and_scope',
    'resident_entity_cache_invalidation',
    'resident_store_global_identity', 'resident_store_reference_write_boundaries', 'resident_store_cross_collection_snapshot',
    'resident_store_models_and_cache', 'resident_store_graph_metadata_and_references',
    'store_wal_canonical_action_frames', 'store_wal_impossible_counts_and_truncation',
    'store_wal_entity_record_replay', 'store_wal_host_index_boundaries', 'store_wal_atomic_resident_refresh',
    'store_wal_recovery_fixture',
    'record_filter_durable_restart', 'contract_write_durable_publication',
    'resident_file_models_and_checkpoint', 'resident_file_owned_restart_fixture',
    'resident_file_rejected_batch_restores_committed_state', 'resident_file_publication_failure_boundaries',
    'resident_file_checkpoint_failure_boundaries', 'resident_file_wal_capacity_preserves_committed_state',
    'catalog_contract_round_trip', 'catalog_legacy_defaults_and_corruption',
    'catalog_schema_resolution', 'catalog_stored_expression_admission',
    'catalog_immutable_cache_and_invalidation', 'catalog_snapshot_recovery',
    'contract_write_defaults_and_schema_modes', 'contract_write_coercion_and_nulls',
    'contract_write_generated_and_checks', 'contract_write_update_plan_and_timestamps',
    'contract_write_unique_keys_and_targets', 'contract_write_models_and_vector_dimensions',
    'contract_write_ttl_overrides_and_bounds',
    'record_fields_aliases_and_identity', 'record_fields_json_and_binary_paths',
    'record_fields_graph_bindings', 'record_filter_nullable_column_lookup',
    'record_filter_exact_values_and_metadata', 'record_filter_predicates_and_errors',
    'record_filter_short_circuit_and_host_errors', 'record_filter_secret_admission',
    'record_order_exact_integer_keys', 'record_order_nullable_keys',
    'record_order_text_prefixes_and_stable_ties', 'record_order_expression_keys_once',
    'record_order_topk_matches_stable_sort', 'record_order_limit_offset_width',
    'record_order_identity_and_secret_callbacks', 'record_order_unordered_and_nan_edges',
    'scalar_geo_aliases_and_operations', 'scalar_geo_argument_forms', 'scalar_geo_input_policy',
    'scalar_dispatch_clock_values', 'scalar_dispatch_time_bucket_inputs',
    'scalar_dispatch_substrings_and_patterns', 'scalar_dispatch_trim_slice_and_quote',
    'scalar_dispatch_arithmetic_and_cast', 'scalar_dispatch_case_and_coalesce_lazy',
    'scalar_dispatch_text_and_lengths', 'scalar_dispatch_money',
    'scalar_dispatch_missing_and_host_fallback',
    'projection_value_legacy_literals', 'projection_value_resolution_and_fallback',
    'projection_value_secret_input_order',
    'record_projection_fields_aliases_and_literals', 'record_projection_star_and_column_layout',
    'record_projection_document_paths', 'record_projection_bindings_and_window_output',
    'record_projection_typed_values_and_labels', 'record_projection_fallback_and_data_errors',
    'record_projection_secret_sensitivity', 'record_projection_output_policy_and_redaction',
    'numeric_scalar_operand_coercion', 'numeric_scalar_arithmetic_types',
    'numeric_scalar_null_and_zero', 'numeric_scalar_timestamp_ms',
    'numeric_scalar_legacy_precision', 'numeric_scalar_cast_fast_paths',
    'numeric_scalar_cast_validation',
    'text_scalar_unicode_and_wide_bounds', 'text_scalar_regex_capture_and_limits',
    'text_scalar_trim_position_and_quote',
    'temporal_duration_units_and_overflow', 'temporal_timestamp_conversion_and_bounds',
    'temporal_bucket_zero_and_wide_values',
    'geo_values_and_coordinate_forms', 'geo_spherical_and_ellipsoidal_math', 'geo_polygon_and_bounds',
    'table_plan_callback_order_and_aliases', 'table_plan_fast_lookup_and_errors',
    'table_plan_wide_bounds_and_topk', 'table_plan_depth_and_first_child',
    'table_planning_order_costs_and_metadata', 'table_planning_document_and_universal_routes',
    'table_planning_effective_sql_and_zero_bounds', 'table_planning_bounded_metadata_and_secrets',
    'runtime_expression_native_values_and_casts', 'runtime_expression_lazy_boolean_and_case',
    'runtime_expression_eager_functions_and_raw_specials', 'runtime_expression_predicates_and_unsupported',
    'runtime_expression_secret_policy_and_depth',
    'value_functions_text_and_missing', 'value_functions_clock_money_and_geo',
    'value_functions_json_and_binary_documents', 'value_functions_json_mutation',
    'value_functions_depth_and_wide_indices', 'value_functions_host_routing',
    'window_ranks_partition_order_and_ties', 'window_lag_lead_values_and_defaults',
    'window_aggregate_frames_and_peers', 'window_wide_offsets_and_bounds',
    'window_errors_and_publication_order', 'window_materialization_admission',
    'window_cross_row_secret_policy', 'window_projection_depth_and_subtraction',
    'aggregate_group_bindings_alias_resolution', 'aggregate_group_bindings_expression_labels',
    'aggregate_group_bindings_shape_and_host_admission', 'aggregate_group_bindings_depth_limits',
    'aggregate_group_bindings_result_metadata',
    'aggregate_results_grouped_columns_and_hidden', 'aggregate_results_empty_input_semantics',
    'aggregate_results_wide_pagination', 'aggregate_results_admission_and_secret_policy',
    'aggregate_planning_aliases_and_hidden_reuse', 'aggregate_planning_dedup_and_legacy_inputs',
    'aggregate_planning_nested_expressions_and_ordering', 'aggregate_planning_parameters_and_pagination',
    'aggregate_planning_signatures_and_unsupported', 'aggregate_planning_depth_bounds',
    'aggregate_shared_numeric_slots', 'aggregate_counts_nulls_and_empty',
    'aggregate_ordered_values_and_distinct', 'aggregate_parameters_and_aliases',
    'aggregate_merge_batches', 'aggregate_numeric_coercion',
    'aggregate_typed_group_keys', 'aggregate_depth_and_host_policy',
  ])
}

before(async () => {
  fixtures = await mkdtemp(resolve(tmpdir(), 'reddb-browser-interop-'))
  const path = resolve(fixtures, 'native.rdb')
  nativeArtifact('create', path)
  nativeFixture = [...await readFile(path)]
  nativeSnapshot = [...await readFile(resolve(root, 'crates/reddb-engine/tests/fixtures/native-store-v12.rdst'))]
  const treePath = resolve(fixtures, 'native-tree.rdb')
  nativeTree('create', treePath)
  nativeTreeFixture = [...await readFile(treePath)]
  const queryPath = resolve(fixtures, 'native-query.rdb')
  execFileSync('cargo', [`+${toolchain}`, 'run', '--quiet', '--locked', '-p', 'reddb-io-engine', '--example', 'browser_stored_query', '--', queryPath], { cwd: root, stdio: 'inherit' })
  nativeQueryFixture = [...await readFile(queryPath)]
  server = createServer(async (request, response) => {
    networkRequests.push(request.url)
    try {
      const path = resolve(root, `.${decodeURIComponent(new URL(request.url, 'http://localhost').pathname)}`)
      if (!path.startsWith(`${root}${sep}`)) { response.writeHead(403).end(); return }
      const type = { '.js': 'text/javascript', '.wasm': 'application/wasm', '.html': 'text/html' }[extname(path)] || 'application/octet-stream'
      const body = await readFile(path)
      const headers = { 'Content-Type': type }
      if (path === resolve(root, 'packages/browser/test/offline-worker.js')) {
        headers['Service-Worker-Allowed'] = '/packages/browser/'
      }
      response.writeHead(200, headers).end(body)
    } catch { response.writeHead(404).end() }
  })
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve))
  origin = `http://127.0.0.1:${server.address().port}`
})

after(async () => {
  if (server) await new Promise((resolve) => server.close(resolve))
  if (fixtures) await rm(fixtures, { recursive: true, force: true })
})

for (const browser of browsers) {
  describe(browser, () => {
    let context
    let profile
    let supportsStorage
    async function launch(options = {}) {
      context = await browserTypes[browser].launchPersistentContext(profile, { headless: true, ...options })
      const probe = await context.newPage()
      await probe.goto(`${origin}/packages/browser/test/harness.html`)
      supportsStorage = await probe.evaluate(async () => {
        if (!navigator.storage?.getDirectory) return false
        const url = URL.createObjectURL(new Blob([
          'postMessage(typeof globalThis.FileSystemFileHandle?.prototype.createSyncAccessHandle === "function")',
        ], { type: 'text/javascript' }))
        const worker = new Worker(url)
        try {
          return await new Promise((resolve, reject) => {
            worker.onmessage = ({ data }) => resolve(data)
            worker.onerror = reject
          })
        } finally {
          worker.terminate()
          URL.revokeObjectURL(url)
        }
      })
      await probe.close()
    }
    async function page() {
      const page = await context.newPage()
      await page.goto(`${origin}/packages/browser/test/harness.html`)
      await page.waitForFunction(() => window.RedDBBrowser)
      return page
    }

    before(async () => { profile = await mkdtemp(resolve(tmpdir(), 'reddb-browser-browser-')); await launch() })
    after(async () => { await context?.close(); await rm(profile, { recursive: true, force: true }) })

    function storageTest(name, run) {
      test(name, async (testContext) => {
        if (!supportsStorage) {
          testContext.skip('This browser port does not expose native synchronous OPFS; Safari/iOS qualification remains pending')
          return
        }
        await run()
      })
    }

    test('shared Rust query, storage and MVCC conformance executes in WASM', async () => {
      const browserPage = await page()
      try {
        await sharedConformance(browserPage)
      } finally { await browserPage.close() }
    })

    test('production runtime executes without storage, preserving canonical values and errors', async () => {
      const current = await page()
      try {
        const result = await current.evaluate(async () => {
          // An accidental storage dependency must fail, even on OPFS-capable ports.
          Object.defineProperty(navigator, 'storage', { value: undefined, configurable: true })
          Object.defineProperty(navigator, 'locks', { value: undefined, configurable: true })
          const runtime = await RedDBBrowser.createRuntime()
          try {
            const info = await runtime.info()
            const cases = await (await fetch('/crates/reddb-engine/tests/scalar-cases.json')).json()
            const actual = []
            for (const { sql } of cases) actual.push(await runtime.executeScalar(sql))
            await runtime.validateQuery('SELECT 1 FROM people')
            const errors = []
            for (const sql of ['SELECT 1 FROM people', 'SELECT 1 / 0', 'SELECT FROM WHERE', 1]) {
              try { await runtime.executeScalar(sql) } catch (error) { errors.push(error.code) }
            }
            return { info, actual, expected: cases.map(({ expected }) => expected), errors }
          } finally { await runtime.close() }
        })
        assert.deepEqual(result.info, {
          abiVersion: 1, valueEncoding: 'reddb-json-v1',
          capabilities: { queryValidation: true, scalarQueries: true, storedQueries: true, storedQueryMode: 'read-write-tables', sqlWrites: true, transactions: true, queryParameters: true, sync: false },
        })
        assert.deepEqual(result.actual, result.expected)
        assert.deepEqual(result.errors, ['QUERY_UNSUPPORTED', 'QUERY_EVALUATION_ERROR', 'QUERY_PARSE_ERROR', 'INVALID_ARGUMENT'])
      } finally { await current.close() }
    })

    test('worker rejects an old SDK ABI before creating an artifact', async () => {
      const current = await page()
      try {
        const result = await current.evaluate(async () => {
          const worker = new Worker('/packages/browser/src/worker.js', { type: 'module' })
          try {
            const reply = await new Promise((resolve, reject) => {
              worker.onmessage = ({ data }) => resolve(data)
              worker.onerror = event => reject(new Error(event.message))
              worker.postMessage({ abiVersion: 0, id: 1, method: 'open', params: { name: 'abi-mismatch' } })
            })
            let created = false
            if (navigator.storage?.getDirectory) {
              const root = await navigator.storage.getDirectory()
              try {
                const directory = await root.getDirectoryHandle('reddb-browser')
                await directory.getFileHandle('abi-mismatch.rdb')
                created = true
              } catch (error) { if (error.name !== 'NotFoundError') throw error }
            }
            return { code: reply.error.code, created }
          } finally { worker.terminate() }
        })
        assert.deepEqual(result, { code: 'VERSION_MISMATCH', created: false })
      } finally { await current.close() }
    })

    storageTest('public table SELECT recovers native snapshot and WAL after an offline browser restart', async () => {
      const current = await page()
      const first = await current.evaluate(async (fixture) => {
        const root = await navigator.storage.getDirectory()
        const directory = await root.getDirectoryHandle('reddb-browser', { create: true })
        const file = await directory.getFileHandle('public-sql.rdb', { create: true })
        const writer = await file.createWritable()
        await writer.write(Uint8Array.from(fixture))
        await writer.close()
        const database = await RedDBBrowser.openDatabase({ name: 'public-sql' })
        const errors = []
        let result
        try {
          result = await database.query('SELECT p.rid AS id, UPPER(name) AS label, total * 2 AS doubled FROM people AS p WHERE nickname IS NULL ORDER BY total DESC LIMIT 4294967296')
          const exact = await database.query('SELECT name FROM people WHERE rid IN (9007199254740993)')
          if (JSON.stringify(exact.rows) !== JSON.stringify([['Ana']])) throw new Error('Unsigned identity IN lost its exact SQL binding')
          for (const sql of ['INSERT INTO people (name) VALUES (\'x\')', 'SELECT COUNT(*) FROM empty', 'SELECT missing FROM empty', 'SELECT total / 0 FROM people', null]) {
            try { await database.query(sql) } catch (error) { errors.push(error.code) }
          }
          try { await RedDBBrowser.openArtifact({ name: 'public-sql' }) } catch (error) { errors.push(error.code) }
          try { await RedDBBrowser.openDatabase({ name: 'public-sql' }) } catch (error) { errors.push(error.code) }
        } finally { await database.close() }
        try { await RedDBBrowser.openDatabase({ name: 'missing-public-sql' }) } catch (error) { errors.push(error.code) }
        const bytes = [...new Uint8Array(await (await file.getFile()).arrayBuffer())]
        await navigator.serviceWorker.register('/packages/browser/test/offline-worker.js', { scope: '/packages/browser/' })
        return { result, errors, unchanged: bytes.every((byte, index) => byte === fixture[index]) && bytes.length === fixture.length }
      }, nativeQueryFixture)
      const expected = { columns: ['id', 'label', 'doubled'], rows: [[2, 'BIA', 44], [{ $uint: '9007199254740993' }, 'ANA', 20]] }
      assert.deepEqual(first.result, expected)
      assert.deepEqual(first.errors, ['QUERY_UNSUPPORTED', 'QUERY_UNSUPPORTED', 'QUERY_EVALUATION_ERROR', 'QUERY_EVALUATION_ERROR', 'INVALID_ARGUMENT', 'FILE_BUSY', 'FILE_BUSY', 'DATABASE_NOT_FOUND'])
      assert.equal(first.unchanged, true, 'public SELECT and failed mutations must leave native bytes intact')
      await current.waitForFunction(() => navigator.serviceWorker.controller)
      await current.close()
      await context.close()
      try {
        await withHostOffline(async () => {
          const requestsBefore = networkRequests.length
          await launch({ offline: true })
          const reopened = await page()
          try {
            const result = await reopened.evaluate(async () => {
              const database = await RedDBBrowser.openDatabase({ name: 'public-sql' })
              try { return await database.query('SELECT p.rid AS id, UPPER(name) AS label, total * 2 AS doubled FROM people AS p WHERE nickname IS NULL ORDER BY total DESC LIMIT 4294967296') }
              finally { await database.close() }
            })
            assert.deepEqual(result, expected)
            assert.equal(networkRequests.length, requestsBefore, 'public SQL must not reach the HTTP host')
          } finally { await reopened.close() }
        })
      } finally { await context.setOffline(false) }
    })

    storageTest('positional parameters retain types through the real Worker and WASM binder', async () => {
      const current = await page()
      try {
        const result = await current.evaluate(async () => {
          const database = await RedDBBrowser.openDatabase({ name: 'sql-params', create: true })
          const text = "O'Reilly'); DELETE FROM bindings; -- $1\0🦊"
          const errors = []
          try {
            await database.query('CREATE TABLE bindings (id INTEGER PRIMARY KEY, body TEXT, flag BOOLEAN, amount FLOAT)')
            await database.query('INSERT INTO bindings VALUES ($1, $2, $3, $4)', [9007199254740993n, text, true, 0.125])
            const first = await database.query('SELECT id, body, flag, amount FROM bindings WHERE id = ?1', [9007199254740993n])
            const typed = await database.query('SELECT $1 AS nil, $2 AS flag, $3 AS minimum, $4 AS maximum, $5 AS fraction FROM bindings', [null, true, -9223372036854775808n, 18446744073709551615n, 0.125])
            const page = await database.query('SELECT id FROM bindings LIMIT $1 OFFSET $2', [1, 0])
            for (const value of [undefined, NaN, Infinity, 9007199254740992, 18446744073709551616n, -9223372036854775809n, {}, [1, 2]]) {
              try { await database.query('SELECT $1 AS invalid FROM bindings', [value]) }
              catch (error) { errors.push(error.code) }
            }
            for (const params of [[], [1, 2]]) {
              try { await database.query('SELECT id FROM bindings WHERE id = $1', params) }
              catch (error) { errors.push(error.code) }
            }
            await database.query('BEGIN')
            await database.query('UPDATE bindings SET body = $1 WHERE id = $2', ['Discard', 9007199254740993n])
            try { await database.query('DELETE FROM bindings WHERE id = $1', []) }
            catch (error) { errors.push(error.code) }
            try { await database.query('COMMIT') } catch (error) { errors.push(error.code) }
            await database.query('ROLLBACK')
            const unchanged = await database.query('SELECT body FROM bindings WHERE id = $1', [9007199254740993n])
            return { first, typed, page, errors, unchanged, capability: (await database.info()).capabilities.queryParameters }
          } finally { await database.close() }
        })
        assert.equal(result.capability, true)
        assert.deepEqual(result.first.rows, [[{ $int: '9007199254740993' }, "O'Reilly'); DELETE FROM bindings; -- $1\0🦊", true, 0.125]])
        assert.deepEqual(result.typed.rows, [[null, true, { $int: '-9223372036854775808' }, { $uint: '18446744073709551615' }, 0.125]])
        assert.deepEqual(result.page.rows, [[{ $int: '9007199254740993' }]])
        assert.deepEqual(result.errors, [...new Array(8).fill('INVALID_ARGUMENT'), 'INVALID_PARAMS', 'INVALID_PARAMS', 'INVALID_PARAMS', 'QUERY_EVALUATION_ERROR'])
        assert.deepEqual(result.unchanged.rows, [["O'Reilly'); DELETE FROM bindings; -- $1\0🦊"]])
      } finally { await current.close() }
    })

    storageTest('native database backups preserve committed state and never overwrite existing files', async () => {
      const current = await page()
      try {
        const result = await current.evaluate(async fixture => {
          const same = (left, right) => left.length === right.length && left.every((byte, index) => byte === right[index])
          const native = await RedDBBrowser.restoreDatabase({ name: 'backup-native', backup: Uint8Array.from(fixture) })
          let nativeMatch
          try {
            nativeMatch = same(await native.exportBackup(), fixture)
            await native.query('SELECT name FROM people')
          } finally { await native.close() }
          const database = await RedDBBrowser.openDatabase({ name: 'backup-source', create: true })
          const errors = []
          const chunks = []
          for (let index = 0; index < 48; index++) chunks.push(String.fromCharCode(...crypto.getRandomValues(new Uint8Array(32768))))
          const text = "O'Reilly\0🦊" + btoa(chunks.join(''))
          let backup, pendingExcluded, stillPending
          try {
            await database.query('CREATE TABLE backups (id INTEGER PRIMARY KEY, body TEXT NOT NULL, revision INTEGER DEFAULT = 7)')
            await database.query('INSERT INTO backups (id, body) VALUES ($1, $2)', [9007199254740993n, text])
            backup = await database.exportBackup()
            await database.query('BEGIN')
            await database.query('UPDATE backups SET body = $1', ['Uncommitted'])
            pendingExcluded = same(backup, await database.exportBackup())
            stillPending = (await database.query('SELECT body FROM backups')).rows[0][0] === 'Uncommitted'
            try { await RedDBBrowser.restoreDatabase({ name: 'backup-source', backup }) }
            catch (error) { errors.push(error.code) }
          } finally { await database.close() }
          const restored = await RedDBBrowser.restoreDatabase({ name: 'backup-restored', backup })
          let restoredMatch, exact, revision
          try {
            restoredMatch = same(backup, await restored.exportBackup())
            const rows = (await restored.query('SELECT id, body, revision FROM backups')).rows
            exact = rows[0][0].$int === '9007199254740993' && rows[0][1] === text
            revision = rows[0][2]
            await restored.query('INSERT INTO backups (id, body) VALUES ($1, $2)', [2, 'after restore'])
          } finally { await restored.close() }
          try { await RedDBBrowser.restoreDatabase({ name: 'backup-restored', backup }) }
          catch (error) { errors.push(error.code) }
          try { await RedDBBrowser.restoreDatabase({ name: 'backup-corrupt', backup: new Uint8Array(32) }) }
          catch (error) { errors.push(error.code) }
          try { await RedDBBrowser.openDatabase({ name: 'backup-corrupt' }) }
          catch (error) { errors.push(error.code) }
          const reopened = await RedDBBrowser.openDatabase({ name: 'backup-restored' })
          let preserved
          try {
            const rows = (await reopened.query('SELECT body, revision FROM backups WHERE id = $1', [2])).rows
            preserved = rows[0][0] === 'after restore' && rows[0][1] === 7
          }
          finally { await reopened.close() }
          return { nativeMatch, pendingExcluded, stillPending, restoredMatch, exact, revision, preserved, errors, length: backup.length }
        }, nativeQueryFixture)
        assert.deepEqual({ ...result, length: undefined }, {
          nativeMatch: true, pendingExcluded: true, stillPending: true, restoredMatch: true,
          exact: true, revision: 7, preserved: true,
          errors: ['FILE_BUSY', 'DATABASE_EXISTS', 'DATABASE_ERROR', 'DATABASE_NOT_FOUND'], length: undefined,
        })
        assert.ok(result.length > 1024 * 1024, 'exercise multi-chunk backup I/O')
      } finally { await current.close() }
    })

    storageTest('public SQL creates a database and worker termination rolls back uncommitted writes', async () => {
      const current = await page()
      try {
        const result = await current.evaluate(async () => {
          const worker = new Worker('/packages/browser/src/worker.js', { type: 'module' })
          const database = await RedDBBrowser.openDatabase({ name: 'sql-crash', create: true, worker })
          await database.query('CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL)')
          await database.query("INSERT INTO notes VALUES (1, 'committed')")
          await database.query('BEGIN')
          await database.query("UPDATE notes SET body = 'uncommitted'")
          await database.query("INSERT INTO notes VALUES (2, 'also uncommitted')")
          worker.terminate()
          await navigator.locks.request('reddb-browser:sql-crash', () => {})
          const reopened = await RedDBBrowser.openDatabase({ name: 'sql-crash' })
          try {
            return await reopened.query('SELECT id, body FROM notes ORDER BY id')
          } finally { await reopened.close() }
        })
        assert.deepEqual(result, { columns: ['id', 'body'], rows: [[1, 'committed']] })
      } finally { await current.close() }
    })

    storageTest('create option preserves an existing corrupt SQL database', async () => {
      const current = await page()
      try {
        const result = await current.evaluate(async () => {
          const root = await navigator.storage.getDirectory()
          const directory = await root.getDirectoryHandle('reddb-browser', { create: true })
          const file = await directory.getFileHandle('sql-corrupt.rdb', { create: true })
          const writer = await file.createWritable()
          await writer.write(new Uint8Array([9, 8, 7]))
          await writer.close()
          let code
          try { await RedDBBrowser.openDatabase({ name: 'sql-corrupt', create: true }) }
          catch (error) { code = error.code }
          return { code, bytes: [...new Uint8Array(await (await file.getFile()).arrayBuffer())] }
        })
        assert.deepEqual(result, { code: 'DATABASE_ERROR', bytes: [9, 8, 7] })
      } finally { await current.close() }
    })

    storageTest('worker executes scalar RQL and preserves exact integers and artifact state', async () => {
      const browserPage = await page()
      try {
        const result = await browserPage.evaluate(async () => {
          const cases = await (await fetch('/crates/reddb-engine/tests/scalar-cases.json')).json()
          const file = await RedDBBrowser.openArtifact({ name: 'scalar' })
          const generation = await file.generation()
          const actual = []
          for (const { sql } of cases) actual.push(await file.executeScalar(sql))
          const errors = []
          for (const sql of ['SELECT 1 FROM people', 'SELECT 1 / 0', 'SELECT FROM WHERE', 1]) {
            try { await file.executeScalar(sql) } catch (error) { errors.push(error.code) }
          }
          const unchanged = generation === await file.generation()
          const wal = await file.readWal()
          await file.close()
          return { actual, expected: cases.map(({ expected }) => expected), errors, unchanged, wal }
        })
        assert.deepEqual(result.actual, result.expected)
        assert.deepEqual(result.errors, ['QUERY_UNSUPPORTED', 'QUERY_EVALUATION_ERROR', 'QUERY_PARSE_ERROR', 'INVALID_ARGUMENT'])
        assert.equal(result.unchanged, true)
        assert.deepEqual(result.wal, [])
      } finally { await browserPage.close() }
    })

    storageTest('canonical RQL validation and OPFS writes survive browser restart', async () => {
      const first = await page()
      const result = await first.evaluate(async () => {
        const file = await RedDBBrowser.openArtifact({ name: 'restart' })
        await file.validateQuery('SELECT 1')
        let invalid
        try { await file.validateQuery('SELECT FROM WHERE') } catch (error) { invalid = error.code }
        await file.append([new Uint8Array([1, 2, 3]), new Uint8Array([4, 5, 6])])
        const generation = await file.generation()
        await file.close()
        return { generation, invalid }
      })
      assert.equal(result.invalid, 'ARTIFACT_ERROR')
      assert.equal(typeof result.generation, 'string')
      await context.close()
      await launch()
      const reopened = await page()
      assert.deepEqual(await reopened.evaluate(async () => {
        const file = await RedDBBrowser.openArtifact({ name: 'restart' })
        const payloads = (await file.readWal()).map((bytes) => [...bytes])
        await file.close()
        return payloads
      }), [[1, 2, 3], [4, 5, 6]])
    })

    storageTest('checkpoint, WAL growth, append and reopen preserve the native artifact', async () => {
      const current = await page()
      const result = await current.evaluate(async () => {
        let file = await RedDBBrowser.openArtifact({ name: 'checkpoint' })
        await file.append([new Uint8Array([7])])
        await file.checkpoint(new TextEncoder().encode('RDSTcheckpoint'), 128 * 1024)
        await file.append([new Uint8Array([8])])
        await file.close()
        file = await RedDBBrowser.openArtifact({ name: 'checkpoint' })
        const snapshot = new TextDecoder().decode(await file.readSnapshot())
        const wal = (await file.readWal()).map((bytes) => [...bytes])
        await file.close()
        const directory = await (await navigator.storage.getDirectory()).getDirectoryHandle('reddb-browser')
        const bytes = new Uint8Array(await (await (await directory.getFileHandle('checkpoint.rdb')).getFile()).arrayBuffer())
        return { snapshot, wal, magic: new TextDecoder().decode(bytes.slice(0, 8)) }
      })
      assert.deepEqual(result, { snapshot: 'RDSTcheckpoint', wal: [[8]], magic: 'RDBSBLK1' })
    })

    storageTest('a real native .rdb can be changed in the browser and reopened natively', async () => {
      const current = await page()
      const bytes = await current.evaluate(async (fixture) => {
        const directory = await (await navigator.storage.getDirectory()).getDirectoryHandle('reddb-browser', { create: true })
        const handle = await directory.getFileHandle('interop.rdb', { create: true })
        const writer = await handle.createWritable()
        await writer.write(new Uint8Array(fixture))
        await writer.close()
        const file = await RedDBBrowser.openArtifact({ name: 'interop' })
        await file.append([new TextEncoder().encode('browser')])
        await file.close()
        return [...new Uint8Array(await (await handle.getFile()).arrayBuffer())]
      }, nativeFixture)
      const path = resolve(fixtures, `${browser}.rdb`)
      await writeFile(path, Buffer.from(bytes))
      nativeArtifact('verify', path)
    })

    storageTest('a second tab cannot write concurrently, and owner teardown releases the lock', async () => {
      const owner = await page()
      const contender = await page()
      await owner.evaluate(async () => {
        window.file = await RedDBBrowser.openArtifact({ name: 'ownership' })
        await file.append([new Uint8Array([9])])
      })
      assert.equal(await contender.evaluate(async () => {
        try { await RedDBBrowser.openArtifact({ name: 'ownership' }); return 'unexpected success' }
        catch (error) { return error.code }
      }), 'FILE_BUSY')
      await owner.close()
      // Browser process teardown releases OPFS and Web Locks asynchronously.
      const recovered = await contender.evaluate(async () => {
        for (let attempt = 0; attempt < 100; attempt++) {
          try {
            const file = await RedDBBrowser.openArtifact({ name: 'ownership' })
            const payloads = (await file.readWal()).map((bytes) => [...bytes])
            await file.close()
            return payloads
          } catch (error) {
            if (error.code !== 'FILE_BUSY') throw error
            await new Promise((resolve) => setTimeout(resolve, 20))
          }
        }
        throw new Error('Owner teardown did not release the file')
      })
      assert.deepEqual(recovered, [[9]])
    })

    storageTest('a corrupt existing file is preserved, with no silent recreate', async () => {
      const current = await page()
      assert.deepEqual(await current.evaluate(async () => {
        const root = await navigator.storage.getDirectory()
        const directory = await root.getDirectoryHandle('reddb-browser', { create: true })
        const handle = await directory.getFileHandle('corrupt.rdb', { create: true })
        const writer = await handle.createWritable()
        await writer.write(new Uint8Array([77, 88, 99]))
        await writer.close()
        let code
        try { await RedDBBrowser.openArtifact({ name: 'corrupt' }) } catch (error) { code = error.code }
        return { code, bytes: [...new Uint8Array(await (await handle.getFile()).arrayBuffer())] }
      }), { code: 'ARTIFACT_ERROR', bytes: [77, 88, 99] })
    })

    storageTest('invalid arguments leave the artifact usable and close rejects new operations', async () => {
      const current = await page()
      assert.deepEqual(await current.evaluate(async () => {
        const file = await RedDBBrowser.openArtifact({ name: 'arguments' })
        let invalid
        try { await file.append(['not bytes']) } catch (error) { invalid = error.code }
        await file.append([new Uint8Array([10])])
        const closing = file.close()
        let closed
        try { await file.readWal() } catch (error) { closed = error.code }
        await closing
        await file.close()
        return { invalid, closed }
      }), { invalid: 'INVALID_ARGUMENT', closed: 'CLOSED' })
    })

    storageTest('cached JS and WASM boot, run shared MVCC, write and reopen after a browser restart entirely offline', async () => {
      const first = await page()
      await first.evaluate(async () => {
        await navigator.serviceWorker.register('/packages/browser/test/offline-worker.js', { scope: '/packages/browser/' })
      })
      await first.waitForFunction(() => navigator.serviceWorker.controller)
      await context.setOffline(true)
      try {
        await withHostOffline(async () => {
          const requestsBefore = networkRequests.length
          await first.evaluate(async () => {
            if (navigator.onLine) throw new Error('Test requires offline mode')
            const file = await RedDBBrowser.openArtifact({ name: 'offline' })
            await file.append([new Uint8Array([10, 20])])
            await file.close()
        })
        await context.close()
        await launch({ offline: true })
        const restarted = await page()
        const result = await restarted.evaluate(async () => {
          const file = await RedDBBrowser.openArtifact({ name: 'offline' })
          const before = (await file.readWal()).map((bytes) => [...bytes])
          await file.append([new Uint8Array([30, 40])])
          const scalar = await file.executeScalar('SELECT 6 * 7 AS answer')
          await file.close()
          const reopened = await RedDBBrowser.openArtifact({ name: 'offline' })
          const after = (await reopened.readWal()).map((bytes) => [...bytes])
          await reopened.close()
          return { online: navigator.onLine, before, after, scalar }
        })
        await sharedConformance(restarted)
        assert.deepEqual(result, {
          online: false,
          before: [[10, 20]],
          after: [[10, 20], [30, 40]],
          scalar: { columns: ['answer'], rows: [[42]] },
        })
        assert.equal(networkRequests.length, requestsBefore, `offline boot and operations reached the HTTP host: ${networkRequests.slice(requestsBefore).join(', ')}`)
        })
      } finally {
        await context.setOffline(false)
      }
    })

    storageTest('native B-tree entities materialize and project across an offline WASM/browser restart, then reopen natively', async () => {
      async function fixture(phase) {
        const current = await page()
        try {
          return await current.evaluate(async ({ phase, fixture }) => {
            const worker = new Worker('/packages/browser/test/btree-worker.js', { type: 'module' })
            try {
              return await new Promise((resolve, reject) => {
                worker.onmessage = ({ data }) => data.error ? reject(new Error(data.error)) : resolve(data)
                worker.onerror = (event) => reject(new Error(event.message))
                worker.postMessage({ phase, fixture: fixture ? Uint8Array.from(fixture) : undefined })
              })
            } finally { worker.terminate() }
          }, { phase, fixture: phase === 0 ? nativeTreeFixture : undefined })
        } finally { await current.close() }
      }
      const first = await page()
      await first.evaluate(async () => {
        await navigator.serviceWorker.register('/packages/browser/test/offline-worker.js', { scope: '/packages/browser/' })
      })
      await first.waitForFunction(() => navigator.serviceWorker.controller)
      await first.close()
      await context.setOffline(true)
      try {
        await withHostOffline(async () => {
          const requestsBefore = networkRequests.length
          assert.equal((await fixture(0)).phase, 0)
          assert.equal((await fixture(1)).phase, 1)
          await context.close()
          await launch({ offline: true })
          const result = await fixture(2)
          const bytes = result.bytes instanceof Uint8Array ? result.bytes : Uint8Array.from(Object.values(result.bytes))
          const path = resolve(fixtures, `${browser}-tree.rdb`)
          await writeFile(path, bytes)
          nativeTree('verify', path)
          assert.equal(networkRequests.length, requestsBefore, `offline B-tree operations reached the HTTP host: ${networkRequests.slice(requestsBefore).join(', ')}`)
        })
      } finally {
        await context.setOffline(false)
      }
    })

    storageTest('native collections and typed metadata recover from an OPFS snapshot after an offline browser restart', async () => {
      async function recover(seed) {
        const current = await page()
        try {
          return await current.evaluate(async (seed) => {
            const file = await RedDBBrowser.openArtifact({ name: 'native-snapshot-recovery' })
            let snapshot
            try {
              if (seed) await file.checkpoint(Uint8Array.from(seed))
              snapshot = await file.readSnapshot()
            } finally { await file.close() }
            const worker = new Worker('/packages/browser/test/snapshot-worker.js', { type: 'module' })
            try {
              const entities = await new Promise((resolve, reject) => {
                worker.onmessage = ({ data }) => data.error ? reject(new Error(data.error)) : resolve(data.entities)
                worker.onerror = (event) => reject(new Error(event.message))
                worker.postMessage(snapshot)
              })
              return { entities, snapshot: [...snapshot] }
            } finally { worker.terminate() }
          }, seed)
        } finally { await current.close() }
      }
      const first = await page()
      await first.evaluate(async () => {
        await navigator.serviceWorker.register('/packages/browser/test/offline-worker.js', { scope: '/packages/browser/' })
      })
      await first.waitForFunction(() => navigator.serviceWorker.controller)
      await first.close()
      await context.setOffline(true)
      try {
        await withHostOffline(async () => {
          const requestsBefore = networkRequests.length
          assert.deepEqual(await recover(nativeSnapshot), { entities: 8, snapshot: nativeSnapshot })
          await context.close()
          await launch({ offline: true })
          assert.deepEqual(await recover(), { entities: 8, snapshot: nativeSnapshot })
          assert.equal(networkRequests.length, requestsBefore, 'offline snapshot recovery reached the HTTP host')
        })
      } finally { await context.setOffline(false) }
    })

    storageTest('collection contracts and stored expressions recover from OPFS after an offline browser restart', async () => {
      async function recover(seed) {
        const current = await page()
        try {
          return await current.evaluate(async (seed) => {
            const worker = new Worker('/packages/browser/test/catalog-worker.js', { type: 'module' })
            const call = (snapshot) => new Promise((resolve, reject) => {
              worker.onmessage = ({ data }) => data.error ? reject(new Error(data.error)) : resolve(data)
              worker.onerror = (event) => reject(new Error(event.message))
              worker.postMessage(snapshot)
            })
            const file = await RedDBBrowser.openArtifact({ name: 'catalog-snapshot-recovery' })
            try {
              if (seed) await file.checkpoint((await call(null)).snapshot)
              const { contracts } = await call(await file.readSnapshot())
              return contracts
            } finally {
              await file.close()
              worker.terminate()
            }
          }, seed)
        } finally { await current.close() }
      }
      const first = await page()
      await first.evaluate(async () => {
        await navigator.serviceWorker.register('/packages/browser/test/offline-worker.js', { scope: '/packages/browser/' })
      })
      await first.waitForFunction(() => navigator.serviceWorker.controller)
      await first.close()
      await context.setOffline(true)
      try {
        await withHostOffline(async () => {
          const requestsBefore = networkRequests.length
          assert.equal(await recover(true), 1)
          await context.close()
          await launch({ offline: true })
          assert.equal(await recover(false), 1)
          assert.equal(networkRequests.length, requestsBefore, 'offline catalog recovery reached the HTTP host')
        })
      } finally { await context.setOffline(false) }
    })

    storageTest('logical store WAL restores models and metadata after an offline OPFS restart', async () => {
      async function recover(seed) {
        const current = await page()
        try {
          return await current.evaluate(async (seed) => {
            const worker = new Worker('/packages/browser/test/store-wal-worker.js', { type: 'module' })
            const call = (payloads) => new Promise((resolve, reject) => {
              worker.onmessage = ({ data }) => data.error ? reject(new Error(data.error)) : resolve(data)
              worker.onerror = (event) => reject(new Error(event.message))
              worker.postMessage(payloads)
            })
            const file = await RedDBBrowser.openArtifact({ name: 'logical-store-recovery' })
            try {
              if (seed) await file.append((await call(null)).actions)
              const payloads = await file.readWal()
              const { entities } = await call(payloads)
              return { entities, actions: payloads.length }
            } finally {
              await file.close()
              worker.terminate()
            }
          }, seed)
        } finally { await current.close() }
      }
      const first = await page()
      await first.evaluate(async () => {
        await navigator.serviceWorker.register('/packages/browser/test/offline-worker.js', { scope: '/packages/browser/' })
      })
      await first.waitForFunction(() => navigator.serviceWorker.controller)
      await first.close()
      await context.setOffline(true)
      try {
        await withHostOffline(async () => {
          const requestsBefore = networkRequests.length
          const seeded = await recover(true)
          assert.ok(seeded.entities >= 8, 'canonical multimodel corpus')
          assert.equal(seeded.actions, 1 + seeded.entities * 3, 'create, delete and restore actions')
          await context.close()
          await launch({ offline: true })
          assert.deepEqual(await recover(false), seeded)
          assert.equal(networkRequests.length, requestsBefore, 'offline logical WAL recovery reached the HTTP host')
        })
      } finally { await context.setOffline(false) }
    })

    storageTest('owned resident store publishes models, fences a failed flush and recovers OPFS offline', async () => {
      async function fixture(phase) {
        const current = await page()
        try {
          return await current.evaluate(async (phase) => {
            const worker = new Worker('/packages/browser/test/resident-file-worker.js', { type: 'module' })
            try {
              return await new Promise((resolve, reject) => {
                worker.onmessage = ({ data }) => data.error ? reject(new Error(data.error)) : resolve(data)
                worker.onerror = (event) => reject(new Error(event.message))
                worker.postMessage({ phase })
              })
            } finally { worker.terminate() }
          }, phase)
        } finally { await current.close() }
      }
      const first = await page()
      await first.evaluate(async () => {
        await navigator.serviceWorker.register('/packages/browser/test/offline-worker.js', { scope: '/packages/browser/' })
      })
      await first.waitForFunction(() => navigator.serviceWorker.controller)
      await first.close()
      await context.setOffline(true)
      try {
        await withHostOffline(async () => {
          const requestsBefore = networkRequests.length
          const seeded = await fixture(0)
          assert.ok(seeded.entities >= 8)
          assert.deepEqual(await fixture(1), { phase: 1, interrupted: true })
          await context.close()
          await launch({ offline: true })
          assert.deepEqual(await fixture(2), { phase: 2, entities: seeded.entities })
          await context.close()
          await launch({ offline: true })
          assert.deepEqual(await fixture(3), { phase: 3, entities: seeded.entities })
          assert.equal(networkRequests.length, requestsBefore, 'offline owned-store publication reached the HTTP host')
        })
      } finally { await context.setOffline(false) }
    })

    storageTest('canonical write admission rejects invalid rows and recovers generated fields offline', async () => {
      async function fixture(phase) {
        const current = await page()
        try {
          return await current.evaluate(async (phase) => {
            const worker = new Worker('/packages/browser/test/contract-write-worker.js', { type: 'module' })
            try {
              return await new Promise((resolve, reject) => {
                worker.onmessage = ({ data }) => data.error ? reject(new Error(data.error)) : resolve(data)
                worker.onerror = (event) => reject(new Error(event.message))
                worker.postMessage({ phase })
              })
            } finally { worker.terminate() }
          }, phase)
        } finally { await current.close() }
      }
      const first = await page()
      await first.evaluate(async () => {
        await navigator.serviceWorker.register('/packages/browser/test/offline-worker.js', { scope: '/packages/browser/' })
      })
      await first.waitForFunction(() => navigator.serviceWorker.controller)
      await first.close()
      await context.setOffline(true)
      try {
        await withHostOffline(async () => {
          const requestsBefore = networkRequests.length
          assert.deepEqual(await fixture(0), { phase: 0, total: 15 })
          await context.close()
          await launch({ offline: true })
          assert.deepEqual(await fixture(1), { phase: 1, total: 20 })
          await context.close()
          await launch({ offline: true })
          assert.deepEqual(await fixture(2), { phase: 2, total: 20 })
          assert.equal(networkRequests.length, requestsBefore, 'offline contract admission reached the HTTP host')
        })
      } finally { await context.setOffline(false) }
    })

    storageTest('canonical projection, WHERE and ORDER BY recover rows after offline WAL and checkpoint restarts', async () => {
      async function fixture(phase) {
        const current = await page()
        try {
          return await current.evaluate(async (phase) => {
            const worker = new Worker('/packages/browser/test/record-filter-worker.js', { type: 'module' })
            try {
              return await new Promise((resolve, reject) => {
                worker.onmessage = ({ data }) => data.error ? reject(new Error(data.error)) : resolve(data)
                worker.onerror = (event) => reject(new Error(event.message))
                worker.postMessage({ phase })
              })
            } finally { worker.terminate() }
          }, phase)
        } finally { await current.close() }
      }
      const first = await page()
      await first.evaluate(async () => {
        await navigator.serviceWorker.register('/packages/browser/test/offline-worker.js', { scope: '/packages/browser/' })
      })
      await first.waitForFunction(() => navigator.serviceWorker.controller)
      await first.close()
      await context.setOffline(true)
      try {
        await withHostOffline(async () => {
          const requestsBefore = networkRequests.length
          assert.deepEqual(await fixture(0), { phase: 0, counts: [1, 1, 1, 4, 4, 2, 2] })
          await context.close()
          await launch({ offline: true })
          assert.deepEqual(await fixture(1), { phase: 1, counts: [1, 2, 0, 4, 3, 2, 2] })
          await context.close()
          await launch({ offline: true })
          assert.deepEqual(await fixture(2), { phase: 2, counts: [1, 2, 0, 4, 3, 2, 2] })
          assert.equal(networkRequests.length, requestsBefore, 'offline record filtering reached the HTTP host')
        })
      } finally { await context.setOffline(false) }
    })

    storageTest('shared pager recovers allocation and checkpoint state after a torn OPFS write offline', async () => {
      async function fixture(phase) {
        const current = await page()
        try {
          return await current.evaluate(async (phase) => {
            const worker = new Worker('/packages/browser/test/pager-worker.js', { type: 'module' })
            try {
              return await new Promise((resolve, reject) => {
                worker.onmessage = ({ data }) => data.error ? reject(new Error(data.error)) : resolve(data)
                worker.onerror = (event) => reject(new Error(event.message))
                worker.postMessage({ phase })
              })
            } finally { worker.terminate() }
          }, phase)
        } finally { await current.close() }
      }
      const first = await page()
      await first.evaluate(async () => {
        await navigator.serviceWorker.register('/packages/browser/test/offline-worker.js', { scope: '/packages/browser/' })
      })
      await first.waitForFunction(() => navigator.serviceWorker.controller)
      await first.close()
      await context.setOffline(true)
      try {
        await withHostOffline(async () => {
          const requestsBefore = networkRequests.length
          assert.deepEqual(await fixture(0), { phase: 0, recovered: false })
          assert.deepEqual(await fixture(1), { phase: 1, interrupted: true })
          await context.close()
          await launch({ offline: true })
          assert.deepEqual(await fixture(2), { phase: 2, recovered: true })
          assert.deepEqual(await fixture(3), { phase: 3, recovered: false })
          assert.equal(networkRequests.length, requestsBefore, `offline pager recovery reached the HTTP host: ${networkRequests.slice(requestsBefore).join(', ')}`)
        })
      } finally {
        await context.setOffline(false)
      }
    })

    test('missing browser storage support returns an explicit error', async () => {
      const current = await page()
      assert.equal(await current.evaluate(async () => {
        if (navigator.storage) Object.defineProperty(navigator.storage, 'getDirectory', { value: undefined })
        try { await RedDBBrowser.openArtifact({ name: 'unsupported' }) }
        catch (error) { return error.code }
      }), 'STORAGE_UNSUPPORTED')
    })
  })
}
