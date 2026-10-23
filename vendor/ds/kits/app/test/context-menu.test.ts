import { flushSync, tick } from "svelte";
import { describe, expect, it } from "vitest";
import { ContextMenu, contextMenu, type ContextMenuEntry } from "../src/index";
import TestGlyph from "./fixtures/TestGlyph.svelte";
import { classes, classesOf, render } from "./mount";

const ITEMS: readonly ContextMenuEntry[] = [
  { id: "rename", label: "Rename" },
  { id: "duplicate", label: "Duplicate" },
];

async function settle(): Promise<void> {
  await tick();
  await new Promise((resolve) => setTimeout(resolve, 10));
  flushSync();
}

async function closeMenu(): Promise<void> {
  document.activeElement?.dispatchEvent(
    new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }),
  );
  await settle();
  await new Promise((resolve) => setTimeout(resolve, 20));
}

describe("the application ContextMenu", () => {
  it("renders shared icon items through aligned decorative slots", async () => {
    const root = render(ContextMenu, {
      triggerLabel: "Deployment alpha actions",
      contentLabel: "Deployment actions",
      items: [
        { id: "rename", label: "Rename", icon: TestGlyph },
        { id: "duplicate", label: "Duplicate" },
      ],
      size: "sm",
    });
    const target = root.querySelector<HTMLElement>("[data-context-menu-trigger]")!;
    target.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
    await settle();

    const rows = [...document.querySelectorAll<HTMLElement>('[role="menuitem"]')];
    const icon = rows[0]!.querySelector<SVGElement>("[data-icon]")!;
    expect(icon.getAttribute("aria-hidden")).toBe("true");
    expect(icon.getAttribute("width")).toBe("var(--reddb-spatial-icon-size-sm)");
    expect(rows.map((row) => row.querySelector("[data-menu-item-icon]") !== null)).toEqual([
      true,
      true,
    ]);
    expect(
      classes(rows[1]!.querySelector("[data-menu-item-icon]")!).has(
        "size-[var(--reddb-spatial-icon-size-sm)]",
      ),
    ).toBe(true);
    expect(rows.map((row) => row.textContent?.trim())).toEqual(["Rename", "Duplicate"]);
    await closeMenu();
  });

  it("makes the context target reachable and openable without a pointer", async () => {
    const root = render(ContextMenu, {
      triggerLabel: "Deployment alpha actions",
      contentLabel: "Deployment actions",
      items: ITEMS,
    });
    const target = root.querySelector<HTMLElement>("[data-context-menu-trigger]")!;

    expect(target.tabIndex).toBe(0);
    expect(target.getAttribute("aria-haspopup")).toBe("menu");
    target.focus();
    target.dispatchEvent(new KeyboardEvent("keydown", { key: "F10", shiftKey: true, bubbles: true, cancelable: true }));
    await settle();

    const surface = document.querySelector<HTMLElement>("[data-context-menu-surface]")!;
    expect(surface.getAttribute("aria-label")).toBe("Deployment actions");
    expect(document.activeElement?.getAttribute("role")).toBe("menuitem");
    await closeMenu();
    expect(document.querySelector("[data-context-menu-surface]")).toBeNull();
  });

  it("opens on the native context-menu gesture with canonical Popover appearance", async () => {
    const root = render(ContextMenu, {
      triggerLabel: "Deployment alpha actions",
      contentLabel: "Deployment actions",
      items: ITEMS,
      size: "sm",
    });
    const target = root.querySelector<HTMLElement>("[data-context-menu-trigger]")!;
    target.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 20, clientY: 30 }));
    await settle();

    const surface = document.querySelector<HTMLElement>("[data-context-menu-surface]")!;
    expect(classes(surface)).toEqual(classesOf(contextMenu({ size: "sm" }).content()));
    expect(classes(surface.querySelector('[role="menuitem"]')!).has("h-[var(--reddb-spatial-control-height-sm)]")).toBe(true);
    await closeMenu();
    expect(document.querySelector("[data-context-menu-surface]")).toBeNull();
  });
});
