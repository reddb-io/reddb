import { describe, expect, it } from "vitest";
import { Steps, steps as stepsAppearance } from "@reddb-io/design-system/base";
import WayfindingConsumer from "./fixtures/WayfindingConsumer.svelte";
import { classes, render } from "./mount";

describe("the Base Steps", () => {
  it("announces the current step and conveys completion without colour", () => {
    const root = render(Steps, {
      items: [
        { id: "account", label: "Account", href: "?step=account", state: "complete" },
        { id: "profile", label: "Profile", state: "current" },
        { id: "confirm", label: "Confirm", state: "upcoming" },
      ],
    });
    const current = root.querySelector<HTMLElement>('[aria-current="step"]')!;
    const complete = root.querySelector<HTMLElement>('[data-step-state="complete"]')!;
    const link = root.querySelector<HTMLAnchorElement>('a[href="?step=account"]')!;

    expect(root.querySelector("ol")).not.toBeNull();
    expect(current.textContent).toContain("Profile");
    expect(complete.textContent).toContain("Completed");
    link.focus();
    expect(document.activeElement).toBe(link);
  });

  it("keeps token appearance live inside a nested appearance scope", () => {
    const scope = render(WayfindingConsumer).querySelector<HTMLElement>("[data-wayfinding-scope]")!;
    const root = scope.querySelector<HTMLElement>("[data-steps]")!;

    expect(classes(root)).toEqual(new Set(stepsAppearance().root().split(/\s+/)));
    expect(classes(root.querySelector("ol")!).has("gap-[var(--reddb-spatial-gap-md)]")).toBe(true);
    expect(scope.getAttribute("data-density")).toBe("compact");
    expect(root.hasAttribute("data-theme")).toBe(false);
    expect(root.hasAttribute("data-color-scheme")).toBe(false);
    expect(root.hasAttribute("data-contrast")).toBe(false);
    expect(root.hasAttribute("data-density")).toBe(false);
  });
});
