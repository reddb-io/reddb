import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";

const repoRoot = path.resolve(import.meta.dirname, "..");

function read(relativePath) {
  return fs.readFileSync(path.join(repoRoot, relativePath), "utf8");
}

test("schema reference documents SECRET and PASSWORD column types", () => {
  const schema = read("docs/reference/schema.md");
  const createTable = read("docs/query/create-table.md");
  const runtimeTests = read("tests/grouped/multimodel_query/integration_entity_query.rs");

  assert.match(schema, /## Sensitive Column Types/);
  assert.match(schema, /\|\s+`SECRET`\s+\|[^|]*AES-256-GCM[^|]*\|[^|]*`SECRET\('plaintext'\)`/);
  assert.match(schema, /\|\s+`PASSWORD`\s+\|[^|]*Argon2id[^|]*\|[^|]*`PASSWORD\('plaintext'\)`/);
  assert.match(schema, /CREATE TABLE service_accounts \(/);
  assert.match(schema, /api_token SECRET NOT NULL/);
  assert.match(schema, /login_password PASSWORD NOT NULL/);
  assert.match(schema, /VERIFY_PASSWORD\(login_password, 'candidate-password'\)/);
  assert.match(schema, /\$secret\.X/);
  assert.match(schema, /SET SECRET/);
  assert.match(schema, /Column-level\s+`SECRET` values encrypt per-row application data/);

  assert.match(createTable, /Sensitive Column Types/);
  assert.match(createTable, /All 50 types/);
  assert.match(createTable, /\/reference\/schema\.md#sensitive-column-types/);

  assert.match(runtimeTests, /INSERT INTO creds \(name, token\) VALUES \('stripe', SECRET\('sk_live_abc'\)\)/);
  assert.match(runtimeTests, /INSERT INTO accounts \(username, pw\) VALUES \('alice', PASSWORD\('MyP@ss123'\)\)/);
  assert.match(runtimeTests, /SELECT VERIFY_PASSWORD\(pw, 'MyP@ss123'\) AS ok FROM accounts/);
});
