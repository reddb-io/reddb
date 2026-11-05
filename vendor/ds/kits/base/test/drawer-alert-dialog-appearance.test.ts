import { describe, expect, it } from "vitest";
import DrawerAlertDialogConsumer from "./fixtures/DrawerAlertDialogConsumer.svelte";
import { classes, render } from "./mount";

describe("Drawer and AlertDialog in a nested appearance scope", () => {
  it("leave Theme, Color Scheme, and Density selection to their consumer", () => {
    const root = render(DrawerAlertDialogConsumer);
    const scope = root.querySelector<HTMLElement>("[data-overlay-appearance-scope]")!;
    const overlays = [
      scope.querySelector("[data-nested-drawer]"),
      scope.querySelector("[data-nested-alert-dialog]"),
    ];

    expect(scope.getAttribute("data-theme")).toBe("marketing");
    expect(scope.getAttribute("data-color-scheme")).toBe("dark");
    expect(scope.getAttribute("data-density")).toBe("compact");
    for (const overlay of overlays) {
      expect(overlay).not.toBeNull();
      expect(overlay!.hasAttribute("data-theme")).toBe(false);
      expect(overlay!.hasAttribute("data-color-scheme")).toBe(false);
      expect(overlay!.hasAttribute("data-density")).toBe(false);
      expect(classes(overlay!).has("p-[var(--reddb-spatial-inset-lg)]")).toBe(true);
    }
  });
});
