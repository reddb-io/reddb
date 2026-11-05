import { flushSync, tick } from "svelte";
import { describe, expect, it } from "vitest";
import { Toolbar, toolbar, type ToolbarItem } from "../src/index";
import { classes, classesOf, render } from "./mount";

const ITEMS: readonly ToolbarItem[] = [
  { id: "bold", label: "Bold" },
  { id: "italic", label: "Italic" },
  { id: "docs", label: "Docs", href: "/docs" },
];

async function settle(): Promise<void> {
  await tick();
  await new Promise((resolve) => setTimeout(resolve, 5));
  flushSync();
}

describe("the application Toolbar", () => {
  it("has one roving tab stop and follows its declared orientation", async () => {
    const root = render(Toolbar, { label: "Formatting", items: ITEMS });
    const bar = root.querySelector<HTMLElement>('[role="toolbar"]')!;
    const controls = [...bar.querySelectorAll<HTMLElement>("[data-toolbar-item]")];

    expect(bar.getAttribute("aria-label")).toBe("Formatting");
    expect(bar.getAttribute("aria-orientation")).toBe("horizontal");
    expect(controls.filter((control) => control.tabIndex === 0)).toHaveLength(1);
    controls[0]!.focus();
    controls[0]!.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true, cancelable: true }));
    await settle();
    expect(document.activeElement).toBe(controls[1]);
  });

  it("keeps links native and routes its spacing through Density roles", () => {
    const root = render(Toolbar, { label: "Formatting", items: ITEMS, size: "sm" });
    const bar = root.querySelector<HTMLElement>('[role="toolbar"]')!;
    const docs = bar.querySelector<HTMLAnchorElement>('a[data-toolbar-item]')!;

    expect(docs.getAttribute("href")).toBe("/docs");
    expect(classes(bar)).toEqual(classesOf(toolbar({ size: "sm" }).root()));
    expect(classes(docs).has("h-[var(--reddb-spatial-control-height-sm)]")).toBe(true);
  });
});
