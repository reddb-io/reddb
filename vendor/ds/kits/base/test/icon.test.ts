import { describe, expect, it, vi } from "vitest";
import { ICON_COLORS, ICON_SIZES, Icon } from "../src/index";
import TestGlyph from "./fixtures/TestGlyph.svelte";
import { render, rendered } from "./mount";

describe("the Base Icon", () => {
  it("binds a lucide-shaped glyph to Density size and semantic color tokens", () => {
    expect(ICON_SIZES).toEqual(["sm", "md", "lg"]);

    const icon = rendered(
      render(Icon, {
        icon: TestGlyph,
        size: "sm",
        color: "primary",
        "aria-label": "Create",
      }),
    );

    expect(icon.hasAttribute("data-icon")).toBe(true);
    expect(icon.getAttribute("width")).toBe("var(--reddb-spatial-icon-size-sm)");
    expect(icon.getAttribute("height")).toBe("var(--reddb-spatial-icon-size-sm)");
    expect(icon.getAttribute("color")).toBe("var(--reddb-color-primary)");
    expect(icon.getAttribute("stroke")).toBe("var(--reddb-color-primary)");
    expect(icon.getAttribute("stroke-width")).toBe("2");
    expect(icon.getAttribute("aria-label")).toBe("Create");
    expect(icon.hasAttribute("data-density")).toBe(false);
  });

  it("inherits the surrounding ink when asked for the current color", () => {
    const icon = rendered(render(Icon, { icon: TestGlyph, color: "current", "aria-hidden": "true" }));

    expect(icon.getAttribute("color")).toBe("currentColor");
    expect(icon.getAttribute("stroke")).toBe("currentColor");
  });

  it("draws the deprecated muted alias in ink-muted and warns once in development", () => {
    // `muted` is a surface role (ink at 8%, ADR 0019): a glyph drawn in it
    // measured 1.19:1. It is out of the vocabulary and kept one release as an
    // alias, so an existing `color="muted"` turns legible instead of breaking.
    expect(ICON_COLORS).not.toContain("muted");
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      rendered(render(Icon, { icon: TestGlyph, color: "ink-muted", "aria-hidden": "true" }));
      expect(warn).not.toHaveBeenCalled();

      const icons = [1, 2].map(() =>
        rendered(render(Icon, { icon: TestGlyph, color: "muted", "aria-hidden": "true" })),
      );
      for (const icon of icons) {
        expect(icon.getAttribute("color")).toBe("var(--reddb-color-ink-muted)");
        expect(icon.getAttribute("stroke")).toBe("var(--reddb-color-ink-muted)");
      }
      expect(warn).toHaveBeenCalledTimes(1);
      expect(String(warn.mock.calls[0]![0])).toMatch(/color="muted" is deprecated.*color="ink-muted"/);
    } finally {
      warn.mockRestore();
    }
  });
});
