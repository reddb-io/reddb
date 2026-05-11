import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";

const repoRoot = path.resolve(import.meta.dirname, "..");

function read(relativePath) {
  return fs.readFileSync(path.join(repoRoot, relativePath), "utf8");
}

test("Vault sealed storage and unseal behavior is covered through public runtime paths", () => {
  const vaultTests = read("tests/e2e_vault_sealed_storage.rs");

  assert.match(vaultTests, /fn vault_put_seals_payload_before_persistence\(\)/);
  assert.match(vaultTests, /CREATE VAULT secrets WITH OWN MASTER KEY/);
  assert.match(vaultTests, /VAULT PUT secrets\.api_key/);
  assert.match(vaultTests, /VAULT GET secrets\.api_key/);
  assert.match(vaultTests, /persistent database artifacts must not contain vault plaintext/);
  assert.match(vaultTests, /metadata read should not require key material/);

  assert.match(vaultTests, /fn vault_get_is_metadata_only_and_unseal_is_capability_gated_and_audited\(\)/);
  assert.match(vaultTests, /UNSEAL VAULT secrets\.api_key/);
  assert.match(vaultTests, /vault:unseal/);
  assert.match(vaultTests, /audit must not include plaintext/);
  assert.match(vaultTests, /outcome.*denied/);
  assert.match(vaultTests, /outcome.*success/);
});

test("Vault lifecycle, redaction, audit, and policy evidence is current", () => {
  const vaultTests = read("tests/e2e_vault_sealed_storage.rs");

  assert.match(vaultTests, /fn vault_lifecycle_versions_history_purge_and_historical_unseal_are_audited\(\)/);
  assert.match(vaultTests, /ROTATE VAULT secrets\.api_key/);
  assert.match(vaultTests, /HISTORY VAULT secrets\.api_key/);
  assert.match(vaultTests, /UNSEAL VAULT secrets\.api_key VERSION 1/);
  assert.match(vaultTests, /DELETE VAULT secrets\.api_key/);
  assert.match(vaultTests, /PURGE VAULT secrets\.api_key/);
  assert.match(vaultTests, /vault:unseal_history/);
  assert.match(vaultTests, /vault:purge/);
  assert.match(vaultTests, /vault\/rotate/);
  assert.match(vaultTests, /vault\/delete/);
  assert.match(vaultTests, /vault\/purge/);
});

test("red.config and red.vault system collections are protected and observable", () => {
  const systemTests = read("tests/e2e_system_config_vault.rs");

  assert.match(systemTests, /fn bootstrap_creates_protected_system_config_and_vault_collections\(\)/);
  assert.match(systemTests, /SELECT name, model, internal FROM red\.collections/);
  assert.match(systemTests, /"red\.config"/);
  assert.match(systemTests, /"red\.vault"/);
  assert.match(systemTests, /fn system_config_and_vault_reject_public_create_drop_and_truncate\(\)/);
  assert.match(systemTests, /CREATE CONFIG red\.config/);
  assert.match(systemTests, /DROP VAULT red\.vault/);
  assert.match(systemTests, /TRUNCATE CONFIG red\.config/);
  assert.match(systemTests, /TRUNCATE VAULT red\.vault/);
  assert.match(systemTests, /system schema is read-only/);

  assert.match(systemTests, /config:red\.config\/mode/);
  assert.match(systemTests, /vault:red\.vault\/api_key/);
  assert.match(systemTests, /red\.secret alias should normalize to red\.vault/);
});

test("Config and Vault WATCH, LIST, and TAGS behavior is covered", () => {
  const observationTests = read("tests/e2e_config_vault_observation.rs");
  const parserTests = read("crates/reddb-server/src/storage/query/parser/tests.rs");

  assert.match(observationTests, /fn list_config_prefix_paginates_values_and_tags\(\)/);
  assert.match(observationTests, /PUT CONFIG app feature_a = 'alpha' TAGS \[scope:prod\]/);
  assert.match(observationTests, /LIST CONFIG app PREFIX feature LIMIT 1 OFFSET 1/);
  assert.match(observationTests, /fn watch_config_events_include_values_only_when_read_is_allowed\(\)/);
  assert.match(observationTests, /config_watch_events_since\("app", "flag", start, 10\)/);
  assert.match(observationTests, /fn list_and_watch_vault_are_metadata_only\(\)/);
  assert.match(observationTests, /VAULT PUT secrets\.api_key = '\{secret\}' TAGS \[scope:prod\]/);
  assert.match(observationTests, /LIST VAULT secrets PREFIX api_ LIMIT 1 OFFSET 1/);
  assert.match(observationTests, /vault_watch_events_since\("secrets", "api_key", start, 10\)/);
  assert.match(observationTests, /vault watch exposed plaintext/);

  assert.match(parserTests, /fn test_parse_vault_list_and_watch\(\)/);
  assert.match(parserTests, /LIST VAULT secrets PREFIX api LIMIT 10 OFFSET 2/);
  assert.match(parserTests, /WATCH VAULT secrets PREFIX api FROM LSN 7/);
});

test("Config and Vault domain-separated API evidence is superseded by current transport slices", () => {
  const routing = read("crates/reddb-server/src/server/routing.rs");
  const handlers = read("crates/reddb-server/src/server/handlers_keyed.rs");
  const mcpTools = read("crates/reddb-server/src/mcp/tools.rs");
  const configClient = read("drivers/js/src/config.js");
  const vaultClient = read("drivers/js/src/vault.js");

  assert.match(routing, /fn v1_keyed_routes_split_kv_config_and_vault_domains\(\)/);
  assert.match(routing, /\/v1\/config\/app\/feature/);
  assert.match(routing, /\/v1\/vault\/secrets\/api_key\/incr/);
  assert.match(routing, /CONFIG does not support TTL/);
  assert.match(handlers, /VAULT counter operations are not supported/);

  assert.match(mcpTools, /name: "reddb_config_put"/);
  assert.match(mcpTools, /name: "reddb_vault_get"/);
  assert.match(mcpTools, /name: "reddb_vault_put"/);
  assert.match(mcpTools, /name: "reddb_vault_unseal"/);

  assert.match(configClient, /export class ConfigClient/);
  assert.match(configClient, /PUT CONFIG/);
  assert.match(configClient, /RESOLVE CONFIG/);
  assert.match(vaultClient, /export class VaultClient/);
  assert.match(vaultClient, /VAULT PUT/);
  assert.match(vaultClient, /UNSEAL VAULT/);
});
