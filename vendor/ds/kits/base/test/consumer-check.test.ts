// The Kit's source, under a consumer's own compiler.
//
// A Kit ships as source (ADR 0002), so the compiler that judges it is the
// consumer's: it can mount, render and pass every test in this suite while
// failing the first `svelte-check` it meets in an application. Issue #50 was
// exactly that, in the application Kit; this is the same guard for the first
// component of this one.
//
// It matters more here than it looks. The Logo reaches a Mark by IMPORTING the
// `.svg`, and which declaration makes that legal is the whole question this
// check answers: the consumer tsconfig brings `vite/client`, the way a real
// application does, and the Kit ships no `*.svg` declaration of its own — a
// wildcard ambient module can only be declared once in a program, so shipping
// one would break a consumer's build with a duplicate-identifier error in a
// file they did not write.

import { spawnSync } from "node:child_process";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { KIT_ROOT } from "../tools/paths";

describe("the Kit's source, under a consumer's own compiler", () => {
  it("type-checks clean — no error, no warning, in any component", () => {
    const svelteCheck = join(KIT_ROOT, "node_modules", ".bin", "svelte-check");
    const { status, stdout, stderr } = spawnSync(
      svelteCheck,
      [
        "--workspace",
        ".",
        "--tsconfig",
        "./tsconfig.consumer.json",
        "--fail-on-warnings",
        "--output",
        "human",
      ],
      { cwd: KIT_ROOT, encoding: "utf8" },
    );

    expect(`${stdout}${stderr}`).toContain("0 errors and 0 warnings");
    expect(status).toBe(0);
  }, 120_000);
});
