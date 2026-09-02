import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";

const repoRoot = path.resolve(import.meta.dirname, "..");

function read(relativePath) {
  return fs.readFileSync(path.join(repoRoot, relativePath), "utf8");
}

/**
 * Regex source matching `uses: <repo>@<tag>` in either of the forms the
 * workflows may use: the bare tag, or a commit SHA pinned with the tag kept in
 * a trailing comment (`@<sha> # <tag>`). Actions are SHA-pinned so a
 * retagged release cannot swap the code a workflow runs; Dependabot keeps the
 * SHA and the comment in step. The assertions below care about *which*
 * release is used, not how it is spelled.
 */
function actionRef(repo, tag) {
  const r = repo.replace(/[.*+?^${}()|[\]\\/]/g, "\\$&");
  const t = tag.replace(/[.*+?^${}()|[\]\\/]/g, "\\$&");
  return `${r}@(?:${t}|[0-9a-f]{40} # ${t})`;
}

function workflowJob(workflow, name) {
  const job = workflow.match(new RegExp(`\\n  ${name}:[\\s\\S]*?(?=\\n  [a-zA-Z0-9_-]+:|\\n$)`));
  return job?.[0] ?? "";
}

test("Python driver CI reports the mirrored lockfile remediation before locked builds", () => {
  const workflow = read(".github/workflows/ci.yml");
  const pythonDriver = workflowJob(workflow, "drivers-python-build");
  const lockGuard = pythonDriver.indexOf("name: Verify drivers/python/Cargo.lock is current");
  const firstLockedBuild = pythonDriver.indexOf("run: cargo check --locked");

  assert.ok(lockGuard >= 0, "Python driver CI must check its mirrored lockfile explicitly");
  assert.ok(lockGuard < firstLockedBuild, "the lockfile guard must run before either locked build");
  assert.match(
    pythonDriver,
    /if ! cargo metadata --locked --format-version 1 > \/dev\/null 2>&1; then/,
  );
  assert.match(
    pythonDriver,
    /echo "::error::drivers\/python\/Cargo\.lock is stale; regenerate with: cd drivers\/python && cargo check"/,
  );
  assert.match(
    pythonDriver,
    /# Regenerate the intentionally separate lockfile with: cd drivers\/python && cargo check/,
  );
});

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
  const publishClient = releaseWorkflow.match(/publish-client-image:[\s\S]*?(?=\n  publish-python-wheels:)/)?.[0] ?? "";
  assert.match(publishDocker, new RegExp(`${actionRef("actions/download-artifact", "v8")}[\\s\\S]*name: linux-x86_64`));
  assert.match(publishDocker, new RegExp(`${actionRef("actions/download-artifact", "v8")}[\\s\\S]*name: linux-aarch64`));
  assert.match(publishDocker, /file: docker\/Dockerfile\.release/);

  for (const [jobName, job, imagePattern] of [
    ["publish-docker", publishDocker, /IMAGE: ghcr\.io\/\$\{\{ github\.repository \}\}/],
    ["publish-client-image", publishClient, /IMAGE: ghcr\.io\/\$\{\{ github\.repository \}\}-client/],
  ]) {
    assert.match(job, /id-token: write/, `${jobName} must allow keyless signing`);
    assert.match(job, new RegExp(`uses: ${actionRef("sigstore/cosign-installer", "v4.1.2")}`), `${jobName} installs Cosign`);
    assert.match(job, new RegExp(`id: build[\\s\\S]*uses: ${actionRef("docker/build-push-action", "v7")}`), `${jobName} exposes build digest`);
    assert.match(job, imagePattern, `${jobName} signs the expected GHCR image`);
    assert.match(job, /DIGEST: \$\{\{ steps\.build\.outputs\.digest \}\}/, `${jobName} signs by digest`);
    assert.match(job, /cosign sign --yes "\$\{IMAGE\}@\$\{DIGEST\}"/, `${jobName} signs with Cosign`);
  }
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
  assert.match(
    script,
    /EXTRA_ASSETS=\(\s+checksums\.txt\s+SHA256SUMS\s+"red-\$\{TAG\}\.spdx\.json"\s+"red-\$\{TAG\}\.cyclonedx\.json"\s+\)/,
  );
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
    assert.match(workflow, new RegExp(`uses: ${actionRef("anchore/sbom-action/download-syft", "v0.24.0")}`));
    assert.match(workflow, /syft-version: v1\.46\.0/);
    assert.match(workflow, /name: Generate source SBOMs/);
    assert.match(workflow, /--exclude '\.\/\.git\/\*\*'/);
    assert.match(workflow, /--exclude '\.\/release\/\*\*'/);
    assert.match(workflow, /--exclude '\.\/release-sbom\/\*\*'/);
    assert.match(workflow, /--source-name RedDB\s+--source-version "\$\{VERSION\}"/);
    assert.match(workflow, /red-\$\{RELEASE_TAG\}\.spdx\.json/);
    assert.match(workflow, /red-\$\{RELEASE_TAG\}\.cyclonedx\.json/);
    assert.match(workflow, /name: Add SBOMs to release assets/);
    assert.match(workflow, /cp release-sbom\/\* release\//);
    assert.match(workflow, /find \. -maxdepth 1 -type f/);
    assert.match(workflow, /-name 'red-\*'/);
    assert.match(workflow, /-name 'red_client-\*'/);
    assert.match(workflow, /! -name '\*\.sha256'/);
    assert.match(workflow, /sort -z/);
    assert.match(workflow, /sha256sum/);
    assert.match(workflow, /> release\/checksums\.txt/);
    assert.match(workflow, /test -s release\/checksums\.txt/);
    assert.match(workflow, /cp release\/checksums\.txt release\/SHA256SUMS/);
    assert.match(workflow, /files: release\/\*/);
    assert.match(workflow, /releases\/download\/.+\/SHA256SUMS/);
    assert.match(workflow, /grep -E '  \(red\|red_client\)-linux-x86_64\$' SHA256SUMS \| sha256sum -c -/);
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
  const checkoutStep = workflow.match(new RegExp(`- uses: actions/checkout@(?:v\\d+|[0-9a-f]{40} # v\\d+)[\\s\\S]*?(?=\\n\\n      - uses: pnpm/action-setup@(?:v[\\d.]+|[0-9a-f]{40} # v[\\d.]+))`))?.[0] ?? "";

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
    new RegExp(`uses: ${actionRef("dtolnay/rust-toolchain", "nightly")}[\\s\\S]*uses: \\./\\.github/actions/install-protoc[\\s\\S]*name: Run \\$\\{\\{ matrix\\.target \\}\\}`),
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

test("on-demand parser fuzz runs as one bounded smoke check", () => {
  const workflow = read(".github/workflows/ci.yml");
  const fuzzParsers = workflowJob(workflow, "fuzz-parsers");

  assert.equal(workflowJob(workflow, "fuzz-targets"), "", "fuzz smoke must not fan out into separate runners");
  assert.match(fuzzParsers, /name: Fuzz Parsers/);
  assert.match(fuzzParsers, /needs: gate/);
  // The job also sits behind the `run_heavy` gate; the contract this test
  // pins is that it stays dispatch-only and opt-in, not the exact prefix.
  assert.match(
    fuzzParsers,
    /if:[^\n]*github\.event_name == 'workflow_dispatch' && inputs\.full_ci/,
  );
  assert.match(fuzzParsers, /timeout-minutes: 15/);
  assert.match(fuzzParsers, /FUZZ_PR_TIME_SECONDS: 30/);
  assert.match(fuzzParsers, /shared-key: ubuntu-fuzz-pr-smoke/);
  assert.match(fuzzParsers, /cargo \+nightly fuzz build --dev/);
  for (const target of ["sql_parser", "migration_parser", "conn_string_parser", "query_with_params"]) {
    assert.match(fuzzParsers, new RegExp(`for target in[\\s\\S]*${target}`));
  }
  assert.match(
    fuzzParsers,
    /cargo \+nightly fuzz run --dev "\$target" -- -max_total_time="\$\{FUZZ_PR_TIME_SECONDS\}" -rss_limit_mb=4096 -malloc_limit_mb=2048/,
  );
});

test("weekly parser fuzz keeps bounded coverage for every smoke target", () => {
  const workflow = read(".github/workflows/parser-fuzz-nightly.yml");
  const fuzz = workflowJob(workflow, "fuzz");

  assert.match(workflow, /cron: "17 3 \* \* 6"/);
  assert.match(workflow, /duration_minutes:[\s\S]*- '15'[\s\S]*- '45'[\s\S]*- '60'/);
  assert.match(fuzz, /runs-on: ubuntu-24\.04/);
  assert.match(fuzz, /timeout-minutes: 75/);
  for (const target of ["sql_parser", "migration_parser", "conn_string_parser", "query_with_params"]) {
    assert.match(fuzz, new RegExp(`- ${target}`));
  }
  assert.match(fuzz, /cargo \+nightly fuzz run \$\{\{ matrix\.target \}\} --[\s\S]*-max_total_time="\$\{FUZZ_DURATION_SECONDS\}"/);
});


test("vendored asset-fetcher copies match the source package byte for byte", () => {
  // `packages/internal-asset-fetcher` is workspace-private, so each
  // publishable package carries its own copy. Nothing enforced that they
  // stayed in sync, and this is the code that downloads a binary which
  // postinstall then executes — a copy missing a hardening change is a copy
  // that installs an unverified binary. Compare rather than trust.
  const sourceDir = "packages/internal-asset-fetcher/src";
  const vendored = [
    "drivers/js/src/internal/asset-fetcher",
    "drivers/js-client/src/internal/asset-fetcher",
    "packages/mcp/src/internal/asset-fetcher",
  ];
  const files = ["index.js", "download.js", "checksum.js", "asset-name.js"];

  for (const dir of vendored) {
    for (const file of files) {
      const vendoredPath = path.join(repoRoot, dir, file);
      if (!fs.existsSync(vendoredPath)) continue;
      assert.equal(
        read(`${dir}/${file}`),
        read(`${sourceDir}/${file}`),
        `${dir}/${file} has drifted from ${sourceDir}/${file}`,
      );
    }
  }
});


test("the Helm chart's appVersion is synced, not hand-maintained", () => {
  // `appVersion` is the image tag `helm install` pulls. It was absent from
  // sync-version.js and had drifted to 0.1.2 — a tag that does not exist on
  // ghcr — so a fresh install pulled a missing image or silently fell back to
  // `latest`. Bumping it once only resets the clock; being a sync target is
  // what stops it drifting again.
  const sync = read("scripts/sync-version.js");
  assert.match(sync, /charts\/reddb\/Chart\.yaml/, "the chart must be a sync target");
  assert.match(sync, /type: 'helm-chart'/, "the chart needs its own target type");
  assert.match(sync, /\^appVersion: "/, "the helm-chart type must rewrite appVersion");

  const chart = read("charts/reddb/Chart.yaml");
  const workspace = read("Cargo.toml");
  const appVersion = chart.match(/^appVersion: "(.+?)"/m)?.[1];
  const version = workspace.match(/^version = "(.+?)"/m)?.[1];
  assert.ok(appVersion, "Chart.yaml must declare appVersion");
  assert.equal(
    appVersion,
    version,
    "Chart.yaml appVersion must match the workspace version, or helm pulls a tag that was never published",
  );
});
