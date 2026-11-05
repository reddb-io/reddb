import { describe, expect, it } from "vitest";
import InlineTextConsumer from "./fixtures/InlineTextConsumer.svelte";
import { classes, render } from "./mount";

describe("inline text composition", () => {
  it("inherits nested appearance axes while keeping Density roles live", () => {
    const root = render(InlineTextConsumer);
    const scope = root.querySelector<HTMLElement>("[data-inline-text-scope]")!;
    const link = scope.querySelector<HTMLElement>("[data-link]")!;
    const kbd = scope.querySelector<HTMLElement>("[data-kbd]")!;
    const codeBlock = scope.querySelector<HTMLElement>("[data-code-block]")!;

    expect(scope.getAttribute("data-theme")).toBe("marketing");
    expect(scope.getAttribute("data-color-scheme")).toBe("dark");
    expect(scope.getAttribute("data-density")).toBe("compact");

    for (const capability of [link, kbd, codeBlock]) {
      expect(capability.hasAttribute("data-theme")).toBe(false);
      expect(capability.hasAttribute("data-color-scheme")).toBe(false);
      expect(capability.hasAttribute("data-density")).toBe(false);
    }

    expect([...classes(codeBlock)].some((name) => name.includes("var(--reddb-spatial"))).toBe(
      false,
    );
    const densityOwned = [...codeBlock.querySelectorAll<HTMLElement>("[class]")].flatMap(
      (element) => [...classes(element)].filter((name) => name.includes("var(--reddb-spatial")),
    );
    expect(densityOwned.length).toBeGreaterThan(0);
  });
});
