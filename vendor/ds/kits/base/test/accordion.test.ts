import { flushSync } from "svelte";
import { describe, expect, it, vi } from "vitest";
import DisclosureFamilyConsumer from "./fixtures/DisclosureFamilyConsumer.svelte";
import DisclosureFamilyContractFailures from "./fixtures/DisclosureFamilyContractFailures.svelte";
import { Accordion, accordion } from "./fixtures/disclosure-family-consumer";
import { classes, classesOf, render, rendered } from "./mount";

const ITEMS = [
  { value: "changes", label: "What changed?" },
  { value: "impact", label: "Who is affected?" },
] as const;

function expandedStateFailures(root: HTMLElement): string[] {
  const controls = [...root.querySelectorAll<HTMLButtonElement>("button")];
  return controls.length > 0 && controls.every((control) => control.hasAttribute("aria-expanded"))
    ? []
    : ["accordion triggers do not expose expanded state"];
}

describe("the Base Accordion", () => {
  it("is available with its Extension Seam through the Base consumer subpath", () => {
    expect(Accordion).toBeDefined();
    expect(accordion).toBeTypeOf("function");
  });

  it("associates every trigger with a region and exposes expanded state", () => {
    const root = rendered(render(Accordion, {
      label: "Release questions",
      items: ITEMS,
      expanded: ["changes"],
    }));
    const controls = [...root.querySelectorAll<HTMLButtonElement>("button[aria-expanded]")];

    expect(root.getAttribute("aria-label")).toBe("Release questions");
    expect(controls.map(({ ariaExpanded }) => ariaExpanded)).toEqual(["true", "false"]);
    expect(root.querySelector(`#${controls[0]!.getAttribute("aria-controls")}`)?.getAttribute("role")).toBe("region");
    expect(expandedStateFailures(root)).toEqual([]);
  });

  it("toggles sections while single mode keeps at most one expanded", () => {
    const onchange = vi.fn();
    const root = rendered(render(Accordion, {
      label: "Release questions",
      items: ITEMS,
      expanded: ["changes"],
      onchange,
    }));
    const controls = [...root.querySelectorAll<HTMLButtonElement>("button[aria-expanded]")];

    controls[1]!.click();
    flushSync();
    expect(controls.map(({ ariaExpanded }) => ariaExpanded)).toEqual(["false", "true"]);
    expect(onchange).toHaveBeenLastCalledWith(["impact"]);
    controls[1]!.click();
    flushSync();
    expect(controls.map(({ ariaExpanded }) => ariaExpanded)).toEqual(["false", "false"]);
  });

  it("keeps token appearance and every appearance axis live in a nested scope", () => {
    const consumer = rendered(render(DisclosureFamilyConsumer, {}));
    const root = consumer.querySelector<HTMLElement>("[data-appearance-scope] [data-accordion]")!;
    const styles = accordion();

    expect(classes(root)).toEqual(classesOf(styles.root()));
    expect(classes(root.querySelector("button")!).has("min-h-[var(--reddb-spatial-control-height-sm)]")).toBe(true);
    expect(root.hasAttribute("data-theme")).toBe(false);
    expect(root.hasAttribute("data-color-scheme")).toBe(false);
    expect(root.hasAttribute("data-density")).toBe(false);
  });
});

describe("the deliberately failing Accordion fixture", () => {
  it("diagnoses an accordion without expanded state", () => {
    const root = rendered(render(DisclosureFamilyContractFailures, {
      failure: "accordion-without-expanded-state",
    }));
    expect(expandedStateFailures(root)).toEqual([
      "accordion triggers do not expose expanded state",
    ]);
  });
});
