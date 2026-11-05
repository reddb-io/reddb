import { describe, expect, it } from "vitest";
import SpecializedEntryConsumer from "./fixtures/SpecializedEntryConsumer.svelte";
import { ControlGroup, controlGroup } from "./fixtures/specialized-entry-consumer";
import { classes, classesOf, render, rendered } from "./mount";

describe("the Base ControlGroup", () => {
  it("is available with its Extension Seam through the Base consumer subpath", () => {
    expect(ControlGroup).toBeDefined();
    expect(controlGroup).toBeTypeOf("function");
  });

  it("provides one accessible name without taking focus from its controls", () => {
    const form = rendered(render(SpecializedEntryConsumer, {}));
    const root = form.querySelector<HTMLElement>('[role="group"][aria-label="Record actions"]')!;
    const controls = [...root.querySelectorAll<HTMLButtonElement>("button")];

    expect(root).not.toBeNull();
    expect(controls).toHaveLength(2);
    expect(root.tabIndex).toBe(-1);
    controls[0]?.focus();
    expect(document.activeElement).toBe(controls[0]);
    controls[1]?.focus();
    expect(document.activeElement).toBe(controls[1]);
  });

  it("supports horizontal and vertical arrangement without owning content", () => {
    const form = rendered(render(SpecializedEntryConsumer, {}));
    const horizontal = form.querySelector<HTMLElement>('[aria-label="Record actions"]')!;
    const vertical = form.querySelector<HTMLElement>('[aria-label="Nested actions"]')!;

    expect(horizontal.dataset.orientation).toBe("horizontal");
    expect(vertical.dataset.orientation).toBe("vertical");
    expect(horizontal.textContent).toContain("Save");
    expect(horizontal.textContent).toContain("Cancel");
  });

  it("wears token-backed appearance and inherits a nested appearance scope", () => {
    const form = rendered(render(SpecializedEntryConsumer, {}));
    const horizontal = form.querySelector<HTMLElement>('[aria-label="Record actions"]')!;
    const nested = form.querySelector<HTMLElement>('[data-appearance-scope] [role="group"]')!;

    expect(classes(horizontal)).toEqual(classesOf(controlGroup({ orientation: "horizontal" })));
    expect(classes(horizontal).has("gap-[var(--reddb-spatial-gap-sm)]")).toBe(true);
    expect(horizontal.hasAttribute("data-theme")).toBe(false);
    expect(horizontal.hasAttribute("data-color-scheme")).toBe(false);
    expect(horizontal.hasAttribute("data-density")).toBe(false);
    expect(nested.hasAttribute("data-theme")).toBe(false);
    expect(nested.hasAttribute("data-color-scheme")).toBe(false);
    expect(nested.hasAttribute("data-density")).toBe(false);
  });
});
