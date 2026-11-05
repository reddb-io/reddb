// The Kit's consumer type check — and the proof that it is worth running.
//
// A check that only ever passes is indistinguishable from no check, so the
// real Kit source and a deliberately unfixed fixture are checked side by side:
// the first must be clean under a strict consumer svelte-check, the second
// must be caught, error by error.
//
// This file IS acceptance criterion 2 of issue #50 ("svelte-check --tsconfig
// run against a strict consumer-style tsconfig over the kit source reports 0
// errors on Button.svelte"), in the same way test/lint.test.ts is issue #7's.
// The fixture carries the exact three errors a consumer's Adoption PR found in
// Button.svelte before this Kit fixed them, which is what makes this a
// regression test and not just a smoke test of somebody else's tool.

import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { consumerCheck } from "../tools/consumer-check";

const here = dirname(fileURLToPath(import.meta.url));
const UNFIXED_TSCONFIG = join(here, "fixtures", "consumer-check", "tsconfig.json");

// svelte-check spawns a compiler over the whole Kit; generous next to a mount
// test, still a fraction of the CI step it replaces.
const TIMEOUT = 120_000;

describe("the Kit's source, under a consumer's own compiler", () => {
  it(
    "type-checks clean — no error, no warning, in any component",
    () => {
      const { diagnostics } = consumerCheck();
      expect(diagnostics.map((diagnostic) => `${diagnostic.file}:${diagnostic.line}  ${diagnostic.message}`)).toEqual(
        [],
      );
    },
    TIMEOUT,
  );
});

describe("the consumer type check", () => {
  it(
    "catches the three errors issue #50 fixed in Button",
    () => {
      const { errors } = consumerCheck(UNFIXED_TSCONFIG);
      const messages = errors.map((error) => error.message);

      // 1. `disabled` declared narrower than the attributes it also inherits.
      expect(messages).toContainEqual(expect.stringContaining("cannot simultaneously extend"));
      expect(messages).toContainEqual(expect.stringContaining("Named property 'disabled'"));
      // 2. The two-armed `rest` spread, as a union the compiler will not compute.
      expect(messages).toContainEqual(expect.stringContaining("union type that is too complex to represent"));
      // 3. …and so not assignable to what the element accepts.
      expect(messages).toContainEqual(expect.stringContaining("HTMLProps<\"button\""));

      // Every error it found is in the fixture: nothing here is being blamed on
      // the real Kit, which the test above requires to be clean.
      expect(errors.every((error) => error.file.endsWith("Unfixed.svelte"))).toBe(true);
    },
    TIMEOUT,
  );
});
