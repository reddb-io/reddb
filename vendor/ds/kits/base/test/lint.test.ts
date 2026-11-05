// The anti-hardcode lint, asked of the Base Kit.
//
// Issue #47's third acceptance criterion. The rule and its counter-examples
// live with the linter (packages/kit-lint), because they are facts about the
// rule; what is a fact about this Kit is that every file it ships is clean and
// that the command CI runs says so with a zero exit code.
//
// The Kit's README promised the rule would arrive here by extraction rather
// than by copy, so this suite also pins the extraction itself: the Base Kit's
// linter IS the application Kit's, reached through the shared package.

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

  it("reads the Kit's components and modules, and not the Marks", () => {
    // A Mark is the Brand's drawing, reproduced unaltered: its hex fills are
    // the drawing itself, and linting them would demand the one edit the
    // licence forbids. So the Logo reaches a Mark as a file rather than as
    // markup, and the lint has nothing in it to object to.
    const files = kitSourceFiles();
    expect(files.some((file) => file.endsWith("Logo.svelte"))).toBe(true);
    expect(files.some((file) => file.endsWith(".svg"))).toBe(false);
  });
});

describe("the lint command", () => {
  const TSX = join(KIT_ROOT, "node_modules", ".bin", "tsx");
  const CLI = join(KIT_ROOT, "tools", "lint-cli.ts");

  it("exits zero on the real Kit", () => {
    const { status, stdout } = spawnSync(TSX, [CLI], { cwd: KIT_ROOT, encoding: "utf8" });
    expect(stdout).toContain("passed");
    expect(status).toBe(0);
  }, 30_000);
});
