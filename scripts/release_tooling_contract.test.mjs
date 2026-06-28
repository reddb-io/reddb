import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";

const repoRoot = path.resolve(import.meta.dirname, "..");

function read(relativePath) {
  return fs.readFileSync(path.join(repoRoot, relativePath), "utf8");
}

function workflowJob(workflow, name) {
  const job = workflow.match(new RegExp(`\\n  ${name}:[\\s\\S]*?(?=\\n  [a-zA-Z0-9_-]+:|\\n$)`));
  return job?.[0] ?? "";
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
  const adr = read(".red/adr/0004-red-client-container-image.md");

  assert.match(dockerfile, /--bin red_client -p reddb-io-client\s+--no-default-features/);
  assert.match(dockerfile, /FROM gcr\.io\/distroless\/static-debian12:nonroot AS runtime/);
  assert.match(dockerfile, /ENTRYPOINT \["\/red_client"\]/);
  assert.match(releaseWorkflow, /publish-client-image:/);
  assert.match(releaseWorkflow, /file: docker\/Dockerfile\.client\.release/);
  assert.match(releaseWorkflow, /ghcr\.io\/\$\{\{ github\.repository \}\}-client/);
  assert.match(adr, /ghcr\.io\/reddb-io\/reddb-client:<version>/);
  assert.match(adr, /Target size: < 10 MB/);
});

test("Docker release images publish from GitHub Actions under reddb-io GHCR only", () => {
  const releaseWorkflow = read(".github/workflows/release.yml");
  const releaseDockerfile = read("docker/Dockerfile.release");
  const dockerHubHost = new RegExp(["docker", "io"].join("\\."));
  const dockerHubSecretPrefix = new RegExp(["DOCKER", "HUB_"].join(""));
  const legacyPersonalGhcrNamespace = new RegExp(["ghcr\\.io/[^\\s'\"]*foratt", "ini"].join(""), "i");

  assert.match(releaseWorkflow, /publish-docker:/);
  assert.match(releaseWorkflow, /ghcr\.io\/\$\{\{ github\.repository \}\}/);
  assert.match(releaseWorkflow, /ghcr\.io\/\$\{\{ github\.repository \}\}-client/);
  assert.match(releaseDockerfile, /COPY .*docker-bin\/\$\{TARGETARCH\}\/red \/usr\/local\/bin\/red/);
  assert.doesNotMatch(releaseDockerfile, /cargo build/);
  assert.doesNotMatch(releaseWorkflow, dockerHubHost);
  assert.doesNotMatch(releaseWorkflow, dockerHubSecretPrefix);
  assert.doesNotMatch(releaseWorkflow, legacyPersonalGhcrNamespace);

  const publishDocker = releaseWorkflow.match(/publish-docker:[\s\S]*?(?=\n  publish-client-image:)/)?.[0] ?? "";
  assert.match(publishDocker, /actions\/download-artifact@v8[\s\S]*name: linux-x86_64/);
  assert.match(publishDocker, /actions\/download-artifact@v8[\s\S]*name: linux-aarch64/);
  assert.match(publishDocker, /file: docker\/Dockerfile\.release/);
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
  assert.match(dockerfile, /cargo fetch --locked/);
  assert.match(dockerfile, /cargo build --release --locked --bin red/);
  assert.doesNotMatch(dockerfile, /echo 'fn main\(\) \{\}'/);
  assert.doesNotMatch(dockerfile, /COPY proto\//);
  assert.doesNotMatch(dockerfile, /COPY benches\//);
  assert.match(compose, /context: \.\.\/\.\./);
});

test("verify-release-assets gates every npm publish on the binary contract (#418)", () => {
  const script = read("scripts/verify-release-assets.sh");
  const workflow = read(".github/workflows/release.yml");
  const runbook = read("docs/release-runbook.md");
  const assetName = read("drivers/js/src/internal/asset-fetcher/asset-name.js");

  for (const suffix of [
    "linux-x86_64",
    "linux-aarch64",
    "linux-armv7",
    "windows-x86_64.exe",
  ]) {
    assert.ok(script.includes(suffix), `verify script lists ${suffix}`);
    assert.ok(assetName.includes(suffix), `asset-name.js still maps to ${suffix}`);
  }
  for (const suffix of ["macos-x86_64", "macos-aarch64"]) {
    assert.ok(assetName.includes(suffix), `asset-name.js still maps optional ${suffix}`);
  }
  assert.match(script, /BINS=\(red red_client\)/);
  assert.match(script, /EXTRA_ASSETS=\(\s+checksums\.txt\s+\)/);
  assert.match(script, /gh release view "\$TAG" --repo "\$REPO" --json assets/);

  assert.match(workflow, /verify-release-assets:/);
  assert.match(workflow, /bash scripts\/verify-release-assets\.sh "\$RELEASE_TAG"/);
  for (const job of [
    "publish-npm",
    "publish-js-driver",
    "publish-js-client",
    "publish-bun-client",
  ]) {
    const re = new RegExp(`${job}:[\\s\\S]*?needs: \\[plan, publish-github, verify-release-assets\\]`);
    assert.match(workflow, re, `${job} must depend on verify-release-assets`);
  }

  assert.match(runbook, /Release asset contract/);
  assert.match(runbook, /checksums\.txt/);
  assert.match(runbook, /verify-release-assets\.sh/);
});

test("release workflows publish aggregate checksum manifests for installers", () => {
  const releaseWorkflow = read(".github/workflows/release.yml");
  const rcWorkflow = read(".github/workflows/release-candidate.yml");

  for (const workflow of [releaseWorkflow, rcWorkflow]) {
    assert.match(workflow, /name: Generate checksum manifest/);
    assert.match(workflow, /find \. -maxdepth 1 -type f/);
    assert.match(workflow, /-name 'red-\*'/);
    assert.match(workflow, /-name 'red_client-\*'/);
    assert.match(workflow, /! -name '\*\.sha256'/);
    assert.match(workflow, /sort -z/);
    assert.match(workflow, /sha256sum/);
    assert.match(workflow, /> release\/checksums\.txt/);
    assert.match(workflow, /test -s release\/checksums\.txt/);
    assert.match(workflow, /files: release\/\*/);
    assert.match(workflow, /releases\/download\/.+\/checksums\.txt/);
    assert.match(workflow, /grep -E '  \(red\|red_client\)-linux-x86_64\$' checksums\.txt \| sha256sum -c -/);
  }
});

test("nightly DR drill workflow uses the current-shell runner and public make target", () => {
  const makefile = read("Makefile");
  const script = read("scripts/drill-nightly.sh");
  const workflow = read(".github/workflows/drill-nightly.yml");

  assert.match(makefile, /\ndrill-nightly:\n\t@\.\/scripts\/drill-nightly\.sh/);
  assert.match(script, /CMD="cargo test --locked --test grouped_chaos_drill_persistence --no-fail-fast drill_"/);
  assert.match(script, /mktemp -t drill-nightly\.XXXXXX\.log/);
  assert.doesNotMatch(script, /mktemp -t reddb-drill-nightly/);
  assert.match(script, /eval "\$CMD" >"\$LOG" 2>&1/);
  assert.doesNotMatch(script, /bash -lc "\$CMD"/);
  assert.match(script, /issue #116/);
  assert.match(workflow, /run: make drill-nightly/);
});

test("changesets checkout uses the default token before release PAT handoff", () => {
  const workflow = read(".github/workflows/changesets.yml");
  const checkoutStep = workflow.match(/- uses: actions\/checkout@v\d+[\s\S]*?(?=\n\n      - uses: pnpm\/action-setup@v\d+)/)?.[0] ?? "";

  assert.match(checkoutStep, /fetch-depth: 0/);
  assert.doesNotMatch(checkoutStep, /\n\s+token:/);
  assert.match(workflow, /GITHUB_TOKEN: \$\{\{ secrets\.RELEASE_PAT \|\| secrets\.GITHUB_TOKEN \}\}/);
});

test("wire coverage gate installs protoc and preserves llvm-cov failures", () => {
  const workflow = read(".github/workflows/wire-coverage.yml");

  assert.match(workflow, /uses: \.\/\.github\/actions\/install-protoc[\s\S]*version: '28\.3'/);
  assert.match(workflow, /set -o pipefail[\s\S]*cargo llvm-cov -p reddb-io-wire/);
  assert.match(workflow, /cargo llvm-cov -p reddb-io-wire[\s\S]*\| tee coverage-summary\.txt/);
});

test("parser fuzz nightly installs protoc before fuzz builds", () => {
  const workflow = read(".github/workflows/parser-fuzz-nightly.yml");

  assert.match(workflow, /uses: \.\/\.github\/actions\/install-protoc[\s\S]*version: '28\.3'/);
  assert.match(
    workflow,
    /uses: dtolnay\/rust-toolchain@nightly[\s\S]*uses: \.\/\.github\/actions\/install-protoc[\s\S]*name: Run \$\{\{ matrix\.target \}\}/,
  );
});

test("chaos and DST workflows use least-privilege GitHub token scopes", () => {
  const ciWorkflow = read(".github/workflows/ci.yml");
  const dstWorkflow = read(".github/workflows/dst-nightly.yml");
  const seedSweep = workflowJob(dstWorkflow, "dst-seed-sweep");
  const storageFaultRecovery = workflowJob(dstWorkflow, "storage-fault-recovery");

  assert.match(seedSweep, /\n  dst-seed-sweep:/);
  assert.match(storageFaultRecovery, /\n  storage-fault-recovery:/);
  assert.match(ciWorkflow, /\npermissions:\n  contents: read\n\n/);
  assert.match(dstWorkflow, /\npermissions:\n  contents: read\n\n/);
  assert.doesNotMatch(seedSweep, /issues: write/);
  assert.match(storageFaultRecovery, /\n    permissions:\n      contents: read\n      issues: write\n/);
});

test("DST storage fault issue creation deduplicates open release blockers", () => {
  const workflow = read(".github/workflows/dst-nightly.yml");
  const storageFaultRecovery = workflowJob(workflow, "storage-fault-recovery");

  assert.match(storageFaultRecovery, /const marker = 'nightly-storage-fault-recovery';/);
  assert.match(storageFaultRecovery, /`Marker: \$\{marker\}`/);
  assert.match(storageFaultRecovery, /github\.paginate\(github\.rest\.issues\.listForRepo/);
  assert.match(storageFaultRecovery, /labels: 'release-blocker'/);
  assert.match(storageFaultRecovery, /issue\.body\?\.includes\(`Marker: \$\{marker\}`\)/);
  assert.match(storageFaultRecovery, /github\.rest\.issues\.create/);

  const existingGuard = storageFaultRecovery.indexOf("if (existing) {");
  const existingReturn = storageFaultRecovery.indexOf("return;", existingGuard);
  const createCall = storageFaultRecovery.indexOf("github.rest.issues.create");

  assert.ok(existingGuard >= 0, "existing issue guard must be present");
  assert.ok(existingReturn > existingGuard, "existing issue branch must return");
  assert.ok(createCall > existingReturn, "issue creation must happen after existing issue guard");
});

test("per-PR parser fuzz matrix has bounded fixed fuzz windows", () => {
  const workflow = read(".github/workflows/ci.yml");
  const fuzzTargets = workflowJob(workflow, "fuzz-targets");
  const fuzzParsers = workflowJob(workflow, "fuzz-parsers");

  assert.match(fuzzTargets, /timeout-minutes: 20/);
  for (const target of ["sql_parser", "migration_parser", "conn_string_parser", "query_with_params"]) {
    assert.match(fuzzTargets, new RegExp(`- ${target}`));
  }
  assert.match(
    fuzzTargets,
    /cargo \+nightly fuzz run \$\{\{ matrix\.target \}\} -- -max_total_time=300 -rss_limit_mb=4096 -malloc_limit_mb=2048/,
  );
  assert.match(fuzzParsers, /name: Fuzz Parsers/);
  assert.match(fuzzParsers, /needs: \[fuzz-targets\]/);
});
