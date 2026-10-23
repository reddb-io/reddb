// The anti-hardcode lint, asked of THIS Kit.
//
// The rule and its counter-examples live with the linter
// (packages/kit-lint/test/lint.test.ts), because they are facts about the rule.
// What is a fact about the application Kit is only this: every file it ships is
// clean, and the command CI runs says so with a zero exit code.
//
// This file IS acceptance criterion 2 of issue #7 ("each component's styling
// resolves through Theme/Tokens variables — the anti-hardcode lint passes on
// the Kit"), in the same way packages/theme/test/lint.test.ts is the Theme
// Layer's.

import { spawnSync } from "node:child_process";
import { join } from "node:path";
import {
  lintKitFiles,
  readInteractionAllowlist,
  readTypeLedger,
  readVocabulary,
  settleInteractionAllowlist,
  settleTypeLedger,
} from "@reddb-io/kit-lint";
import { describe, expect, it } from "vitest";
import { KIT_ROOT, kitSourceFiles } from "../tools/paths";

describe("anti-hardcode lint", () => {
  it("passes on every file of the real Kit", () => {
    const files = kitSourceFiles();
    expect(files.length).toBeGreaterThan(0);
    // Type debt the Kit carried when the type rule arrived is deferred by its
    // ledger (ADR 0025); nothing else is, and no ledger entry may outlive its debt.
    const outcome = settleTypeLedger(lintKitFiles(files, readVocabulary()), KIT_ROOT, readTypeLedger(KIT_ROOT), files);
    // Rule 8 (wave 6A): an interactive element that draws its own states is
    // justified in the Kit's interaction allowlist, which may not outlive it.
    const interaction = settleInteractionAllowlist(outcome.violations, KIT_ROOT, readInteractionAllowlist(KIT_ROOT), files);
    expect(interaction.violations).toEqual([]);
    expect(outcome.stale).toEqual([]);
    expect(interaction.stale).toEqual([]);
  });
});

describe("the lint command", () => {
  // A rule CI cannot act on is not a contract, so the exit code is asserted
  // rather than the library's return value. That the command can also FAIL is
  // the linter's own test to make, against a fixture built to break it.
  const TSX = join(KIT_ROOT, "node_modules", ".bin", "tsx");
  const CLI = join(KIT_ROOT, "tools", "lint-cli.ts");

  it("exits zero on the real Kit", () => {
    const { status, stdout } = spawnSync(TSX, [CLI], { cwd: KIT_ROOT, encoding: "utf8" });
    expect(stdout).toContain("passed");
    expect(status).toBe(0);
  }, 30_000);
});
