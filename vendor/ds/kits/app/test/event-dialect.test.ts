// One event dialect (ADR 0026), proven through the application Kit's own
// components.
//
// A value control is `bind:value` plus `onvaluechange(value)`; a disclosure
// or overlay is `bind:open` plus `onopenchange(open)`; any other bindable
// state `<state>` reports through `on<state>change`; a discrete event is a
// lowercase `on<noun>`. The App names the dialect replaced (`onDismiss`,
// `values`/`onvalueschange`) stay one release as aliases that still work and
// warn once in development. `onselect` (item activation) is the sanctioned
// exception and is untouched.

import { flushSync, tick } from "svelte";
import { afterEach, beforeEach, describe, expect, it, vi, type MockInstance } from "vitest";
import {
  CategoryFilter,
  CommandPalette,
  ContextMenu,
  Menubar,
  Pill,
  SplitView,
  SpeedDial,
} from "../src/index";
import { click, render, rendered } from "./mount";

const OPTIONS = [
  { value: "shirts", label: "Shirts" },
  { value: "shoes", label: "Shoes" },
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

async function escape(): Promise<void> {
  press(document.activeElement ?? document.body, "Escape");
  await settle();
}

let warn: MockInstance<typeof console.warn>;
beforeEach(() => {
  warn = vi.spyOn(console, "warn").mockImplementation(() => {});
});
afterEach(() => {
  warn.mockRestore();
});

/** The DS's own deprecation warnings (Svelte's dev diagnostics are not the subject here). */
const warnings = () =>
  warn.mock.calls.map(([message]) => String(message)).filter((message) => message.startsWith("[reddb "));

describe("the canonical callbacks", () => {
  it("reports a dismissal through ondismiss", () => {
    const ondismiss = vi.fn();
    const pill = rendered(render(Pill, { ondismiss }));
    click(pill.querySelector<HTMLButtonElement>("[data-pill-dismiss]")!);
    expect(ondismiss).toHaveBeenCalledTimes(1);
    expect(warnings()).toEqual([]);
  });

  it("reports CategoryFilter's active values through onvaluechange", () => {
    const onvaluechange = vi.fn();
    const root = rendered(
      render(CategoryFilter, { legend: "Categories", options: OPTIONS, value: ["shirts"], onvaluechange }),
    );
    click(root.querySelectorAll<HTMLInputElement>('input[type="checkbox"]')[1]!);
    expect(onvaluechange).toHaveBeenLastCalledWith(["shirts", "shoes"]);
    click(root.querySelectorAll<HTMLInputElement>('input[type="checkbox"]')[0]!);
    expect(onvaluechange).toHaveBeenLastCalledWith(["shoes"]);
    expect(warnings()).toEqual([]);
  });

  it("reports SplitView's fraction through onfractionchange, by keyboard and not when nothing moved", () => {
    const onfractionchange = vi.fn();
    const root = rendered(render(SplitView, { fraction: 0.5, onfractionchange }));
    const divider = root.querySelector<HTMLElement>("[data-split-view-divider]")!;
    press(divider, "ArrowRight");
    expect(onfractionchange).toHaveBeenCalledTimes(1);
    expect(onfractionchange.mock.calls[0]![0]).toBeGreaterThan(0.5);
    // A key the splitter does not own moves nothing and reports nothing.
    press(divider, "ArrowUp");
    expect(onfractionchange).toHaveBeenCalledTimes(1);
  });

  it("reports open state through onopenchange on CommandPalette, SpeedDial and ContextMenu", async () => {
    const paletteOpen = vi.fn();
    const palette = render(CommandPalette, {
      triggerLabel: "Search commands",
      contentLabel: "Commands",
      label: "Command",
      commands: [{ id: "deploy", label: "Deploy" }],
      onopenchange: paletteOpen,
    });
    palette.querySelector<HTMLButtonElement>("[data-popover-trigger]")!.click();
    await settle();
    expect(paletteOpen).toHaveBeenLastCalledWith(true);
    // Choosing a command closes the palette, and the callback hears that too.
    const input = document.querySelector<HTMLInputElement>('[role="combobox"]')!;
    press(input, "ArrowDown");
    await settle();
    press(input, "Enter");
    await settle();
    expect(paletteOpen).toHaveBeenLastCalledWith(false);

    const dialOpen = vi.fn();
    const dial = render(SpeedDial, {
      triggerLabel: "Create",
      contentLabel: "Create actions",
      actions: [{ id: "doc", label: "Document" }],
      onopenchange: dialOpen,
    });
    dial.querySelector<HTMLButtonElement>("[data-popover-trigger]")!.click();
    await settle();
    expect(dialOpen).toHaveBeenLastCalledWith(true);
    await escape();
    expect(dialOpen).toHaveBeenLastCalledWith(false);

    const menuOpen = vi.fn();
    const menu = render(ContextMenu, {
      triggerLabel: "Deployment actions",
      contentLabel: "Deployment actions",
      items: [{ id: "rename", label: "Rename" }],
      onopenchange: menuOpen,
    });
    menu
      .querySelector<HTMLElement>("[data-context-menu-trigger]")!
      .dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));
    await settle();
    expect(menuOpen).toHaveBeenLastCalledWith(true);
    await escape();
    expect(menuOpen).toHaveBeenLastCalledWith(false);
    expect(warnings()).toEqual([]);
  });

  it("reports the open menu of a Menubar through onvaluechange", async () => {
    const onvaluechange = vi.fn();
    const root = render(Menubar, {
      label: "Editor commands",
      menus: [{ id: "file", label: "File", items: [{ id: "new", label: "New file" }] }],
      onvaluechange,
    });
    const trigger = root.querySelector<HTMLElement>("[data-menubar-trigger]")!;
    trigger.focus();
    press(trigger, "Enter");
    await settle();
    expect(onvaluechange).toHaveBeenLastCalledWith("file");
    await escape();
    expect(onvaluechange).toHaveBeenLastCalledWith("");
  });
});

describe("the deprecated aliases (one release)", () => {
  it("still fire, and warn once per name in development", () => {
    const onDismiss = vi.fn();
    for (const _ of [1, 2]) {
      click(rendered(render(Pill, { onDismiss })).querySelector<HTMLButtonElement>("[data-pill-dismiss]")!);
    }
    expect(onDismiss).toHaveBeenCalledTimes(2);

    const onvalueschange = vi.fn();
    const root = rendered(
      render(CategoryFilter, { legend: "Categories", options: OPTIONS, values: ["shirts"], onvalueschange }),
    );
    const boxes = () => root.querySelectorAll<HTMLInputElement>('input[type="checkbox"]');
    expect([...boxes()].map(({ checked }) => checked)).toEqual([true, false]);
    click(boxes()[1]!);
    expect(onvalueschange).toHaveBeenLastCalledWith(["shirts", "shoes"]);
    // The aliased `values` keeps driving the filter for one release.
    expect([...boxes()].map(({ checked }) => checked)).toEqual([true, true]);

    expect(warnings()).toEqual([
      expect.stringMatching(/\[reddb Pill\] onDismiss is deprecated.*ondismiss/),
      expect.stringMatching(/\[reddb CategoryFilter\] values is deprecated.*bind:value/),
      expect.stringMatching(/\[reddb CategoryFilter\] onvalueschange is deprecated.*onvaluechange/),
    ]);
  });
});
