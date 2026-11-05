import { createRawSnippet, flushSync } from "svelte";
import { describe, expect, it } from "vitest";
import { Tooltip, tooltip as tooltipAppearance } from "./fixtures/dialog-tooltip-consumer";
import { classes, classesOf, render } from "./mount";

describe("the Base Tooltip", () => {
  it("opens from keyboard focus and describes an independently named trigger", () => {
    const root = render(Tooltip, {
      label: "Copy connection string",
      content: "Copies the value to your clipboard",
      children: createRawSnippet(() => ({ render: () => "<span>Copy</span>" })),
    });
    const trigger = root.querySelector<HTMLButtonElement>("[data-tooltip-trigger]")!;

    trigger.focus();
    flushSync();

    const tooltip = document.body.querySelector<HTMLElement>('[role="tooltip"]')!;
    expect(trigger.getAttribute("aria-label")).toBe("Copy connection string");
    expect(trigger.getAttribute("aria-describedby")).toBe(tooltip.id);
    expect(tooltip.textContent).toBe("Copies the value to your clipboard");
  });

  it("dismisses with Escape without moving focus", () => {
    const root = render(Tooltip, { label: "Database status", content: "Healthy" });
    const trigger = root.querySelector<HTMLButtonElement>("[data-tooltip-trigger]")!;
    trigger.focus();
    flushSync();

    trigger.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    flushSync();

    expect(document.body.querySelector('[role="tooltip"]')).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  it("opens for a pointer and closes when the pointer leaves", () => {
    const root = render(Tooltip, { label: "Region help", content: "Choose the nearest region" });
    const trigger = root.querySelector<HTMLButtonElement>("[data-tooltip-trigger]")!;

    trigger.dispatchEvent(new MouseEvent("mouseenter"));
    flushSync();
    expect(document.body.querySelector('[role="tooltip"]')?.textContent).toBe(
      "Choose the nearest region",
    );

    trigger.dispatchEvent(new MouseEvent("mouseleave"));
    flushSync();
    expect(document.body.querySelector('[role="tooltip"]')).toBeNull();
  });

  it("wears its exported token appearance and honors reduced motion", () => {
    const root = render(Tooltip, { label: "Status help", content: "Updated every minute" });
    root.querySelector<HTMLButtonElement>("[data-tooltip-trigger]")!.focus();
    flushSync();
    const element = document.body.querySelector('[role="tooltip"]')!;

    expect(classes(element)).toEqual(classesOf(tooltipAppearance()));
    expect(classes(element).has("motion-reduce:transition-none")).toBe(true);
  });
});
