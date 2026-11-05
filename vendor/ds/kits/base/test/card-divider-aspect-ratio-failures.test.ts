import { readFileSync } from "node:fs";
import { join } from "node:path";
import { lintKitSource, readVocabulary } from "@reddb-io/kit-lint";
import { describe, expect, it } from "vitest";
import SurfaceSeparationContractFailures from "./fixtures/SurfaceSeparationContractFailures.svelte";
import { render } from "./mount";

const FIXTURE = join(
  import.meta.dirname,
  "fixtures",
  "SurfaceSeparationContractFailures.svelte",
);

describe("the deliberately failing Card and Divider fixtures", () => {
  it("diagnoses Card spacing frozen against Density", () => {
    const violations = lintKitSource(readFileSync(FIXTURE, "utf8"), FIXTURE, readVocabulary());
    expect(violations.map((violation) => violation.found)).toContain("p-4");
  });

  it("diagnoses a visual Divider without separator semantics", () => {
    const root = render(SurfaceSeparationContractFailures, {
      failure: "missing-separator-semantics",
    });
    const divider = root.querySelector<HTMLElement>("[data-broken-divider]")!;

    expect(divider.getAttribute("role")).not.toBe("separator");
    expect(divider.hasAttribute("aria-orientation")).toBe(false);
  });
});
