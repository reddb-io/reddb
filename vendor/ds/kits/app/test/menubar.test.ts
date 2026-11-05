import { flushSync, tick } from "svelte";
import { describe, expect, it } from "vitest";
import { Menubar, menubar, type MenubarMenu } from "../src/index";
import TestGlyph from "./fixtures/TestGlyph.svelte";
import { classes, classesOf, render } from "./mount";

const MENUS: readonly MenubarMenu[] = [
  { id: "file", label: "File", items: [{ id: "new", label: "New file" }, { id: "open", label: "Open" }] },
  { id: "edit", label: "Edit", items: [{ id: "undo", label: "Undo" }] },
];

async function settle(): Promise<void> {
  await tick();
  await new Promise((resolve) => setTimeout(resolve, 10));
  flushSync();
}

function press(element: Element, key: string): void {
  element.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }));
  flushSync();
}

describe("the application Menubar", () => {
  it("renders shared icon items through aligned decorative slots", async () => {
    const root = render(Menubar, {
      label: "Editor commands",
      menus: [
        {
          id: "file",
          label: "File",
          items: [
            { id: "new", label: "New", icon: TestGlyph },
            { id: "open", label: "Open" },
          ],
        },
      ],
      size: "lg",
    });
    const trigger = root.querySelector<HTMLElement>("[data-menubar-trigger]")!;
    trigger.focus();
    press(trigger, "Enter");
    await settle();

    const surface = document.querySelector<HTMLElement>("[data-menubar-surface]")!;
    const rows = [...surface.querySelectorAll<HTMLElement>('[role="menuitem"]')];
    const icon = rows[0]!.querySelector<SVGElement>("[data-icon]")!;
    expect(icon.getAttribute("aria-hidden")).toBe("true");
    expect(icon.getAttribute("width")).toBe("var(--reddb-spatial-icon-size-lg)");
    expect(rows.map((row) => row.querySelector("[data-menu-item-icon]") !== null)).toEqual([
      true,
      true,
    ]);
    expect(
      classes(rows[1]!.querySelector("[data-menu-item-icon]")!).has(
        "size-[var(--reddb-spatial-icon-size-lg)]",
      ),
    ).toBe(true);
    expect(rows.map((row) => row.textContent?.trim())).toEqual(["New", "Open"]);
  });

  it("provides one named horizontal menu with roving top-level focus", async () => {
    const root = render(Menubar, { label: "Editor commands", menus: MENUS });
    const bar = root.querySelector<HTMLElement>('[role="menubar"]')!;
    const triggers = [...bar.querySelectorAll<HTMLElement>("[data-menubar-trigger]")];

    expect(bar.getAttribute("aria-label")).toBe("Editor commands");
    expect(triggers.filter((trigger) => trigger.tabIndex === 0)).toHaveLength(1);
    triggers[0]!.focus();
    press(triggers[0]!, "ArrowRight");
    await settle();
    expect(document.activeElement).toBe(triggers[1]);
  });

  it("opens a Popover-composed menu and selects its focused command", async () => {
    const selected: string[] = [];
    const root = render(Menubar, {
      label: "Editor commands",
      menus: [{ id: "file", label: "File", items: [{ id: "new", label: "New", onselect: () => selected.push("new") }] }],
    });
    const trigger = root.querySelector<HTMLElement>("[data-menubar-trigger]")!;
    trigger.focus();
    press(trigger, "Enter");
    await settle();

    const surface = document.querySelector<HTMLElement>("[data-menubar-surface]")!;
    const item = surface.querySelector<HTMLElement>('[role="menuitem"]')!;
    expect(classes(surface)).toEqual(classesOf(menubar().content()));
    expect(document.activeElement).toBe(item);
    press(item, "Enter");
    await settle();
    expect(selected).toEqual(["new"]);
  });
});
