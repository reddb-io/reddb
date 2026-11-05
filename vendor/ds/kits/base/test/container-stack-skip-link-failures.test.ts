import { readFileSync } from "node:fs";
import { join } from "node:path";
import { lintKitSource, readVocabulary } from "@reddb-io/kit-lint";
import { describe, expect, it } from "vitest";
import ContainerStackSkipLinkFailures from "./fixtures/ContainerStackSkipLinkFailures.svelte";
import { render } from "./mount";

const FIXTURE = join(import.meta.dirname, "fixtures", "ContainerStackSkipLinkFailures.svelte");

describe("the deliberately failing layout and keyboard-bypass fixtures", () => {
  it("diagnoses Container and Stack spacing frozen against Density", () => {
    const violations = lintKitSource(readFileSync(FIXTURE, "utf8"), FIXTURE, readVocabulary());
    const found = violations.map((violation) => violation.found);

    expect(found).toContain("px-4");
    expect(found).toContain("gap-2");
  });

  it("diagnoses a keyboard bypass that stays hidden and targets nothing", () => {
    const root = render(ContainerStackSkipLinkFailures, { failure: "unusable-bypass" });
    const link = root.querySelector<HTMLAnchorElement>("[data-broken-skip-link]")!;
    link.focus();

    const failures = [
      link.classList.contains("focus:not-sr-only") ? undefined : "focused link remains hidden",
      root.querySelector(link.hash) ? undefined : "bypass target does not exist",
    ].filter(Boolean);

    expect(failures).toEqual(["focused link remains hidden", "bypass target does not exist"]);
  });
});
