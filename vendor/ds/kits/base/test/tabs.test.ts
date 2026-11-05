import { flushSync, tick } from "svelte";
import { describe, expect, it, vi } from "vitest";
import DisclosureFamilyConsumer from "./fixtures/DisclosureFamilyConsumer.svelte";
import DisclosureFamilyContractFailures from "./fixtures/DisclosureFamilyContractFailures.svelte";
import TabsFragmentConsumer from "./fixtures/TabsFragmentConsumer.svelte";
import { Tabs, tabs } from "./fixtures/disclosure-family-consumer";
import { classes, classesOf, render, rendered } from "./mount";

const ITEMS = [
  { value: "summary", label: "Summary" },
  { value: "events", label: "Events" },
  { value: "disabled", label: "Disabled", disabled: true },
] as const;

function press(element: Element, key: string): void {
  element.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }));
  flushSync();
}

function arrowNavigationFailures(root: HTMLElement): string[] {
  const controls = [...root.querySelectorAll<HTMLButtonElement>('[role="tab"]')];
  controls[0]?.focus();
  if (controls[0]) press(controls[0], "ArrowRight");
  return document.activeElement === controls[1] && controls[1]?.ariaSelected === "true"
    ? []
    : ["tabs do not move focus and selection with arrow keys"];
}

describe("the Base Tabs", () => {
  it("is available with its Extension Seam through the Base consumer subpath", () => {
    expect(Tabs).toBeDefined();
    expect(tabs).toBeTypeOf("function");
  });

  it("names a single-selected tablist and associates each tab with its panel", () => {
    const root = rendered(render(Tabs, { label: "Deployment view", items: ITEMS }));
    const controls = [...root.querySelectorAll<HTMLButtonElement>('[role="tab"]')];
    const panel = root.querySelector<HTMLElement>('[role="tabpanel"]')!;

    expect(root.getAttribute("aria-label")).toBe("Deployment view");
    expect(controls.map(({ ariaSelected }) => ariaSelected)).toEqual(["true", "false", "false"]);
    expect(controls.map(({ tabIndex }) => tabIndex)).toEqual([0, -1, -1]);
    expect(panel.getAttribute("aria-labelledby")).toBe(controls[0]!.id);
    expect(controls[0]!.getAttribute("aria-controls")).toBe(panel.id);
  });

  it("uses arrow, Home, and End keys with roving focus and skips disabled tabs", () => {
    const onchange = vi.fn();
    const root = rendered(render(Tabs, { label: "Deployment view", items: ITEMS, onchange }));
    const controls = [...root.querySelectorAll<HTMLButtonElement>('[role="tab"]')];

    controls[0]!.focus();
    press(controls[0]!, "ArrowRight");
    expect(document.activeElement).toBe(controls[1]);
    expect(controls[1]!.ariaSelected).toBe("true");
    press(controls[1]!, "End");
    expect(document.activeElement).toBe(controls[1]);
    press(controls[1]!, "Home");
    expect(document.activeElement).toBe(controls[0]);
    expect(onchange).toHaveBeenCalledWith("events");
  });

  it("keeps token appearance and every appearance axis live in a nested scope", () => {
    const consumer = rendered(render(DisclosureFamilyConsumer, {}));
    const root = consumer.querySelector<HTMLElement>("[data-appearance-scope] [data-tabs]")!;
    const styles = tabs();

    expect(classes(root.querySelector('[role="tablist"]')!)).toEqual(classesOf(styles.list()));
    expect(classes(root.querySelector('[role="tab"]')!).has("h-[var(--reddb-spatial-control-height-sm)]")).toBe(true);
    expect(root.hasAttribute("data-theme")).toBe(false);
    expect(root.hasAttribute("data-color-scheme")).toBe(false);
    expect(root.hasAttribute("data-density")).toBe(false);
  });

  it("selects and focuses the panel containing a fragment target", async () => {
    const root = rendered(render(TabsFragmentConsumer));
    const controls = [...root.querySelectorAll<HTMLButtonElement>('[role="tab"]')];

    try {
      location.hash = "#source-graph";
      window.dispatchEvent(new HashChangeEvent("hashchange"));
      await tick();
      flushSync();

      await vi.waitFor(() => {
        expect(controls[1]?.ariaSelected).toBe("true");
        expect(document.activeElement?.id).toBe("source-graph");
      });
    } finally {
      history.replaceState(null, "", location.pathname);
    }
  });
});

describe("the deliberately failing Tabs fixture", () => {
  it("diagnoses tabs without arrow-key navigation", () => {
    const root = render(DisclosureFamilyContractFailures, {
      failure: "tabs-without-arrow-navigation",
    });
    expect(arrowNavigationFailures(root)).toEqual([
      "tabs do not move focus and selection with arrow keys",
    ]);
  });
});

describe("the Tabs selection language", () => {
  const redFill = (names: Set<string>): boolean =>
    [...names].some((name) => /(?:^|:)bg-primary(?:\/|$)/.test(name) || /(?:^|:)text-on-primary$/.test(name));
  
  it("marks the active tab with a neutral surface and a 2px ink underline, never a red fill, outline or edge (wave 6A)", () => {
    const root = rendered(render(Tabs, { label: "Deployment", items: ITEMS, value: "summary" }));
    const triggers = [...root.querySelectorAll<HTMLElement>('[role="tab"]')];
    const active = triggers.find((trigger) => trigger.getAttribute("aria-selected") === "true")!;

    for (const trigger of triggers) {
      const names = classes(trigger);
      // Every tab carries the same bottom edge, so selection moves nothing.
      expect(names.has("border-b-2")).toBe(true);
      expect(names.has("border-transparent")).toBe(true);
      expect(names.has("border")).toBe(false);
      expect(names.has("data-[state=active]:border-foreground")).toBe(true);
      expect([...names].some((name) => /border(?:-[bstexy])?-primary/.test(name))).toBe(false);
      expect(names.has("data-[state=active]:bg-foreground/10")).toBe(true);
      expect(names.has("data-[state=active]:font-medium")).toBe(true);
      expect(names.has("data-[state=active]:text-foreground")).toBe(true);
      expect(redFill(names)).toBe(false);
    }
    expect(active.getAttribute("data-state")).toBe("active");
  });
});
