import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const workflow = readFileSync(join(root, ".github/workflows/release.yml"), "utf8");
const verifier = readFileSync(join(root, "scripts/verify-release.sh"), "utf8");

test("maintenance releases publish under a line-specific npm dist-tag", () => {
  assert.match(workflow, /npm_dist_tag: \$\{\{ steps\.plan\.outputs\.npm_dist_tag \}\}/);
  assert.match(workflow, /NPM_DIST_TAG="maintenance-1\.36"/);
  assert.equal(
    (workflow.match(/TAG="\$\{\{ needs\.plan\.outputs\.npm_dist_tag \}\}"/g) ?? []).length,
    6,
  );
  assert.doesNotMatch(workflow, /echo "next" \|\| echo "latest"/);
});

test("maintenance container publishing cannot move major or latest aliases", () => {
  assert.match(workflow, /move_global_aliases: \$\{\{ steps\.plan\.outputs\.move_global_aliases \}\}/);
  assert.match(workflow, /MOVE_GLOBAL_ALIASES=false/);
  assert.equal(
    (workflow.match(/pattern=\{\{major\}\}.*needs\.plan\.outputs\.move_global_aliases == 'true'/g) ?? []).length,
    2,
  );
  assert.equal(
    (workflow.match(/value=latest.*needs\.plan\.outputs\.move_global_aliases == 'true'/g) ?? []).length,
    2,
  );
  assert.equal(
    (workflow.match(/flavor:\s*\|\s*\n\s*latest=false/g) ?? []).length,
    2,
    "docker metadata auto-latest must be disabled for both published images",
  );
  assert.equal(
    (workflow.match(/pattern=\{\{major\}\}\.\{\{minor\}\}/g) ?? []).length,
    2,
    "the 1.36 moving alias must remain available",
  );
});

test("global alias repair is explicit, digest-pinned and race-safe", () => {
  assert.match(workflow, /repair_global_aliases:\n\s+type: boolean/);
  assert.match(workflow, /inputs\.repair_global_aliases && inputs\.preview_platforms/);
  assert.match(workflow, /RESTORE_GLOBAL_LATEST/);
  assert.match(workflow, /a maintenance release cannot become global latest/);
  assert.match(workflow, /latest changed concurrently.*refusing repair/);
  assert.match(workflow, /cosign verify "\$\{IMAGE\}@\$\{EXPECTED\}"/);
  assert.match(workflow, /imagetools create --tag "\$\{IMAGE\}:latest" "\$\{IMAGE\}@\$\{EXPECTED\}"/);
  assert.match(workflow, /"v\$\{VERSION\}" "\$\{VERSION\}" "\$\{MAJOR\}"/);
});

test("maintenance GitHub releases are never promoted as repository latest", () => {
  assert.match(workflow, /make_latest: \$\{\{ needs\.plan\.outputs\.move_global_aliases \}\}/);
});

test("release verification resolves the requested npm version, not the latest dist-tag", () => {
  assert.match(verifier, /npm view @reddb-io\/cli@\$VERSION version/);
  assert.doesNotMatch(verifier, /npm view @reddb-io\/cli version/);
  assert.match(verifier, /MAINTENANCE_NPM/);
  assert.match(verifier, /MINOR_DIGEST=.*IMAGE}:1\.36/);
  assert.match(verifier, /MAJOR_DIGEST=.*IMAGE}:1/);
  assert.match(verifier, /LATEST_DIGEST=.*IMAGE}:latest/);
});
