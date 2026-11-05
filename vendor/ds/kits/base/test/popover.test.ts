import { createRawSnippet, flushSync, tick } from "svelte";
import { describe, expect, it } from "vitest";
import { Popover, popover as popoverAppearance } from "@reddb-io/design-system/base";
import { classes, classesOf, render } from "./mount";

async function settle(): Promise<void> {
  await tick();
  await new Promise((resolve) => setTimeout(resolve, 10));
  flushSync();
}

function surface(): HTMLElement | null {
  return document.querySelector("[data-popover-surface]");
}

describe("the Base Popover", () => {
  it("opens a labeled dialog from the canonical Button trigger", async () => {
    const root = render(Popover, {
      triggerLabel: "Deployment actions",
      contentLabel: "Deployment actions",
      children: createRawSnippet(() => ({
        render: () => '<button type="button" data-action>Restart deployment</button>',
      })),
    });
    const trigger = root.querySelector<HTMLButtonElement>("[data-popover-trigger]")!;

    expect(trigger.tagName).toBe("BUTTON");
    expect(trigger.getAttribute("aria-label")).toBe("Deployment actions");
    expect(trigger.getAttribute("aria-haspopup")).toBe("dialog");
    expect(classes(trigger).has("inline-flex")).toBe(true);

    trigger.click();
    await settle();

    expect(trigger.getAttribute("aria-expanded")).toBe("true");
    expect(surface()?.getAttribute("role")).toBe("dialog");
    expect(surface()?.getAttribute("aria-label")).toBe("Deployment actions");
    expect(trigger.getAttribute("aria-controls")).toBe(surface()?.id);
  });

  it("dismisses with Escape and returns focus to its trigger", async () => {
    const root = render(Popover, {
      triggerLabel: "Open filters",
      contentLabel: "Filters",
      children: createRawSnippet(() => ({
        render: () => '<button type="button" data-filter>Apply filters</button>',
      })),
    });
    const trigger = root.querySelector<HTMLButtonElement>("[data-popover-trigger]")!;
    trigger.focus();
    trigger.click();
    await settle();

    const action = document.querySelector<HTMLButtonElement>("[data-filter]")!;
    expect(document.activeElement).toBe(action);
    action.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    await settle();

    expect(surface()).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  it("uses collision-aware positioning and its exported token appearance", async () => {
    render(Popover, {
      triggerLabel: "Open details",
      contentLabel: "Details",
      open: true,
      collisionPadding: 12,
      class: "max-w-md",
    });
    await settle();
    const element = surface()!;

    expect(element.dataset.avoidCollisions).toBe("true");
    expect(element.dataset.collisionPadding).toBe("12");
    expect(classes(element)).toEqual(classesOf(popoverAppearance({ class: "max-w-md" })));
    expect(classes(element).has("motion-reduce:transition-none")).toBe(true);
  });
});
