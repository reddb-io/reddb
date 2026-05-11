import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";

const repoRoot = path.resolve(import.meta.dirname, "..");

function read(relativePath) {
  return fs.readFileSync(path.join(repoRoot, relativePath), "utf8");
}

test("red_client size guard is wired to a documented local and CI budget check", () => {
  const budget = read("crates/reddb-client/SIZE_BUDGET").trim();
  const sizeScript = read("scripts/check-red-client-size.sh");
  const ciWorkflow = read(".github/workflows/ci.yml");
  const clientReadme = read("crates/reddb-client/README.md");

  assert.match(budget, /^[0-9]+$/);
  assert.match(sizeScript, /cargo build --locked --release --bin "\$BIN_NAME" -p reddb-io-client --no-default-features/);
  assert.match(sizeScript, /strip -s "\$stripped"/);
  assert.match(sizeScript, /size > budget/);
  assert.match(ciWorkflow, /red_client size budget[\s\S]*run: \.\/scripts\/check-red-client-size\.sh/);
  assert.match(clientReadme, /SIZE_BUDGET[\s\S]*scripts\/check-red-client-size\.sh/);
});

test("red_client container release contract uses the thin client Dockerfile and package", () => {
  const dockerfile = read("Dockerfile.client");
  const releaseWorkflow = read(".github/workflows/release.yml");
  const adr = read("docs/adr/0004-red-client-container-image.md");

  assert.match(dockerfile, /--bin red_client -p reddb-io-client\s+--no-default-features/);
  assert.match(dockerfile, /FROM gcr\.io\/distroless\/static-debian12:nonroot AS runtime/);
  assert.match(dockerfile, /ENTRYPOINT \["\/red_client"\]/);
  assert.match(releaseWorkflow, /publish-client-image:/);
  assert.match(releaseWorkflow, /file: Dockerfile\.client/);
  assert.match(releaseWorkflow, /ghcr\.io\/\$\{\{ github\.repository \}\}-client/);
  assert.match(adr, /ghcr\.io\/reddb-io\/reddb-client:<version>/);
  assert.match(adr, /Target size: < 10 MB/);
});

test("Docker release images publish from GitHub Actions under reddb-io GHCR only", () => {
  const releaseWorkflow = read(".github/workflows/release.yml");
  const dockerHubHost = new RegExp(["docker", "io"].join("\\."));
  const dockerHubSecretPrefix = new RegExp(["DOCKER", "HUB_"].join(""));
  const legacyPersonalNamespace = new RegExp(["foratt", "ini"].join(""), "i");

  assert.match(releaseWorkflow, /publish-docker:/);
  assert.match(releaseWorkflow, /ghcr\.io\/\$\{\{ github\.repository \}\}/);
  assert.match(releaseWorkflow, /ghcr\.io\/\$\{\{ github\.repository \}\}-client/);
  assert.doesNotMatch(releaseWorkflow, dockerHubHost);
  assert.doesNotMatch(releaseWorkflow, dockerHubSecretPrefix);
  assert.doesNotMatch(releaseWorkflow, legacyPersonalNamespace);
});

test("release workflow uses runnable toolchain and pack commands", () => {
  const releaseWorkflow = read(".github/workflows/release.yml");

  assert.doesNotMatch(releaseWorkflow, /1\.100\.0/);
  assert.doesNotMatch(releaseWorkflow, /pnpm pack --dry-run/);
  assert.match(releaseWorkflow, /pnpm pack --pack-destination "\$RUNNER_TEMP"/);
});

test("main Docker image builds from files present in the repository", () => {
  const dockerfile = read("Dockerfile");
  const compose = read("testdata/compose/replica.yml");

  assert.match(dockerfile, /COPY crates\/ crates\//);
  assert.doesNotMatch(dockerfile, /COPY proto\//);
  assert.doesNotMatch(dockerfile, /COPY benches\//);
  assert.match(compose, /context: \.\.\/\.\./);
});

test("nightly DR drill workflow uses the current-shell runner and public make target", () => {
  const makefile = read("Makefile");
  const script = read("scripts/drill-nightly.sh");
  const workflow = read(".github/workflows/drill-nightly.yml");

  assert.match(makefile, /\ndrill-nightly:\n\t@\.\/scripts\/drill-nightly\.sh/);
  assert.match(script, /CMD="cargo test --locked --test 'drill_\*' --no-fail-fast"/);
  assert.match(script, /eval "\$CMD" >"\$LOG" 2>&1/);
  assert.doesNotMatch(script, /bash -lc "\$CMD"/);
  assert.match(script, /issue #116/);
  assert.match(workflow, /run: make drill-nightly/);
});
