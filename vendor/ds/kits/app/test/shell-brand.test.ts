import { describe, expect, it } from "vitest";
import ShellBrandComposition from "./fixtures/ShellBrandComposition.svelte";
import { render, rendered } from "./mount";

describe("the application shell brand composition", () => {
  it.each([
    { navbar: true, rail: true, expectedRegion: "navbar" },
    { navbar: false, rail: true, expectedRegion: "rail" },
    { navbar: false, rail: false, expectedRegion: "panel" },
  ])(
    "renders exactly one brand in the $expectedRegion region",
    ({ navbar, rail, expectedRegion }) => {
      const shell = rendered(render(ShellBrandComposition, { navbar, rail }));
      const marks = [...shell.querySelectorAll("[data-shell-brand-mark]")];

      expect(marks).toHaveLength(1);
      expect(marks[0]?.closest("[data-shell-brand-region]")?.getAttribute(
        "data-shell-brand-region",
      )).toBe(expectedRegion);
    },
  );
});
