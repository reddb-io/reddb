import { describe, expect, it } from "vitest";
import { KBD_SIZES, Kbd, kbd as kbdAppearance } from "@reddb-io/design-system/base";
import { classes, classesOf, render, rendered } from "./mount";

describe("the Base Kbd", () => {
  it("renders a chord as semantic key caps with hidden visual separators", () => {
    const chord = rendered(
      render(Kbd, { keys: ["Ctrl", "Shift", "K"], separator: "+" }),
    );
    const caps = [...chord.querySelectorAll("kbd")];
    const separators = [...chord.querySelectorAll("[aria-hidden='true']")];

    expect(chord.tagName).toBe("KBD");
    expect(caps.map((cap) => cap.textContent)).toEqual(["Ctrl", "Shift", "K"]);
    expect(separators.map((separator) => separator.textContent)).toEqual(["+", "+"]);
    expect(chord.tabIndex).toBe(-1);
  });

  it("offers typography-owned sizes without selecting an appearance axis", () => {
    expect(KBD_SIZES).toEqual(["sm", "md"]);

    for (const size of KBD_SIZES) {
      const chord = rendered(render(Kbd, { keys: ["K"], size }));
      const cap = chord.querySelector("kbd")!;

      expect(classes(cap)).toEqual(classesOf(kbdAppearance({ size })));
      expect(cap.hasAttribute("data-theme")).toBe(false);
      expect(cap.hasAttribute("data-color-scheme")).toBe(false);
      expect(cap.hasAttribute("data-density")).toBe(false);
    }
  });
});
