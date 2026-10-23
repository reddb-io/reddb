import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import { DRAWER_SIDES, Drawer, drawer as drawerAppearance } from "./fixtures/drawer-alert-dialog-consumer";
import { classes, classesOf, render } from "./mount";

describe("the Base Drawer", () => {
  it("moves focus into the drawer and restores it after keyboard dismissal", () => {
    const root = render(Drawer, {
      triggerLabel: "Open filters",
      title: "Filters",
      children: createRawSnippet(() => ({
        render: () => '<button data-first-filter type="button">Apply filters</button>',
      })),
    });
    const trigger = root.querySelector<HTMLButtonElement>("[data-dialog-trigger]")!;
    trigger.focus();

    trigger.click();
    const dialog = root.querySelector<HTMLDialogElement>("dialog")!;
    expect(document.activeElement).toBe(root.querySelector("[data-first-filter]"));

    dialog.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(dialog.open).toBe(false);
    expect(document.activeElement).toBe(trigger);
  });

  // Which edge a side paints at is a rendered fact a class name cannot prove:
  // `right-0` once passed here while the panel opened on the left. The showcase
  // browser test (apps/showcase/test/drawer.browser.test.ts) measures it.
  it("renders every side with its exported appearance", () => {
    expect(DRAWER_SIDES).toEqual(["top", "right", "bottom", "left"]);

    for (const side of DRAWER_SIDES) {
      const root = render(Drawer, {
        triggerLabel: `Open ${side} drawer`,
        title: `${side} drawer`,
        side,
        class: "consumer-drawer",
      });
      const element = root.querySelector("dialog")!;

      expect(classes(element)).toEqual(
        classesOf(drawerAppearance({ side, class: "consumer-drawer" })),
      );
      expect(classes(element).has("p-[var(--reddb-spatial-inset-lg)]")).toBe(true);
      expect(classes(element).has("motion-reduce:transition-none")).toBe(true);
    }
  });
});
