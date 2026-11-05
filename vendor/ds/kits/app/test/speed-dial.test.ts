import { flushSync, tick } from "svelte";
import { describe, expect, it } from "vitest";
import { SpeedDial, speedDial, type SpeedDialAction } from "../src/index";
import { classes, classesOf, render } from "./mount";

const ACTIONS: readonly SpeedDialAction[] = [
  { id: "note", label: "New note" },
  { id: "upload", label: "Upload", href: "/upload" },
];

async function settle(): Promise<void> {
  await tick();
  await new Promise((resolve) => setTimeout(resolve, 10));
  flushSync();
}

describe("the application SpeedDial", () => {
  it("opens its actions in the canonical Popover focus lifecycle", async () => {
    const root = render(SpeedDial, {
      triggerLabel: "Create",
      contentLabel: "Create actions",
      actions: ACTIONS,
    });
    const trigger = root.querySelector<HTMLButtonElement>("[data-popover-trigger]")!;
    expect(trigger.getAttribute("aria-haspopup")).toBe("dialog");
    trigger.focus();
    trigger.click();
    await settle();

    const surface = document.querySelector<HTMLElement>("[data-popover-surface]")!;
    const actions = [...surface.querySelectorAll<HTMLElement>("[data-speed-dial-action]")];
    expect(actions).toHaveLength(2);
    expect(document.activeElement).toBe(actions[0]);
    expect(actions[1]!.tagName).toBe("A");
  });

  it("leaves arrangement local while exposing Density-aware action appearance", async () => {
    const root = render(SpeedDial, {
      triggerLabel: "Create",
      contentLabel: "Create actions",
      actions: ACTIONS,
      size: "sm",
      side: "top",
      align: "end",
    });
    root.querySelector<HTMLButtonElement>("[data-popover-trigger]")!.click();
    await settle();

    const surface = document.querySelector<HTMLElement>("[data-popover-surface]")!;
    expect(classes(surface)).toEqual(classesOf(speedDial({ size: "sm" }).content()));
    expect(classes(surface.querySelector("[data-speed-dial-action]")!).has("min-h-[var(--reddb-spatial-control-height-sm)]")).toBe(true);
  });
});
