import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";

const repoRoot = path.resolve(import.meta.dirname, "..");

function read(relativePath) {
  return fs.readFileSync(path.join(repoRoot, relativePath), "utf8");
}

test("migration branch conflict contract is explicitly scoped to current runtime behavior", () => {
  const overview = read("docs/migrations/overview.md");
  const vcsIntegration = read("docs/migrations/vcs-integration.md");
  const migrationTests = read("tests/e2e_migrations_bootstrap.rs");

  assert.match(overview, /Branch-scoped migration visibility is not implemented/);
  assert.match(overview, /`?red_migrations`? is a global system collection/);
  assert.match(vcsIntegration, /`?MigrationConflict`? is not emitted by `?red vcs merge`? today/);
  assert.match(vcsIntegration, /Use explicit `?DEPENDS ON`? edges after both migration definitions exist/);
  assert.match(migrationTests, /migration_registration_is_global_across_vcs_branches/);
});
