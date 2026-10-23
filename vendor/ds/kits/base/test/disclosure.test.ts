import { flushSync } from "svelte";
import { describe, expect, it, vi } from "vitest";
import DisclosureFamilyConsumer from "./fixtures/DisclosureFamilyConsumer.svelte";
import { Disclosure, disclosure } from "./fixtures/disclosure-family-consumer";
import { classes, classesOf, render, rendered } from "./mount";

describe("the Base Disclosure", () => {
  it("is available with its Extension Seam through the Base consumer subpath", () => {
    expect(Disclosure).toBeDefined();
    expect(disclosure).toBeTypeOf("function");
  });

  it("associates a canonical trigger with its expandable region", () => {
    const root = rendered(render(Disclosure, { label: "Advanced details", open: true }));
    const trigger = root.querySelector<HTMLButtonElement>("button")!;
    const region = root.querySelector<HTMLElement>('[role="region"]')!;

    expect(classes(trigger).has("inline-flex")).toBe(true);
    expect(trigger.ariaExpanded).toBe("true");
    expect(trigger.getAttribute("aria-controls")).toBe(region.id);
    expect(region.getAttribute("aria-labelledby")).toBe(trigger.id);
  });

  it("toggles visible content and reports the controlled state", () => {
    const onchange = vi.fn();
    const root = rendered(render(Disclosure, {
      label: "Advanced details",
      open: true,
      onchange,
    }));
    const trigger = root.querySelector<HTMLButtonElement>("button")!;

    trigger.click();
    flushSync();
    expect(trigger.ariaExpanded).toBe("false");
    expect(root.querySelector('[role="region"]')).toBeNull();
    expect(onchange).toHaveBeenCalledWith(false);
  });

  it("keeps token appearance and every appearance axis live in a nested scope", () => {
    const consumer = rendered(render(DisclosureFamilyConsumer, {}));
    const root = consumer.querySelector<HTMLElement>("[data-appearance-scope] [data-disclosure]")!;
    const styles = disclosure();

    expect(classes(root)).toEqual(classesOf(styles.root()));
    expect(classes(root.querySelector("button")!).has("min-h-[var(--reddb-spatial-control-height-sm)]")).toBe(true);
    expect(root.hasAttribute("data-theme")).toBe(false);
    expect(root.hasAttribute("data-color-scheme")).toBe(false);
    expect(root.hasAttribute("data-density")).toBe(false);
  });
});
