import { flushSync, tick } from "svelte";
import { describe, expect, it } from "vitest";
import {
  DropdownMenu,
  dropdownMenuGroups,
  dropdownMenuItems,
  popover as popoverAppearance,
  type DropdownMenuEntry,
} from "@reddb-io/design-system/base";
import TestGlyph from "./fixtures/TestGlyph.svelte";
import { classes, classesOf, render } from "./mount";

const ITEMS: readonly DropdownMenuEntry[] = [
  { id: "profile", label: "Profile", shortcut: ["Ctrl", "P"] },
  { id: "settings", label: "Settings", href: "/settings" },
  {
    heading: "Danger zone",
    items: [
      { id: "archive", label: "Archive" },
      { id: "delete", label: "Delete", disabled: true },
    ],
  },
];

async function settle(): Promise<void> {
  const deadline = Date.now() + 3_000;
  let previous: Element | null = null;
  let still = 0;
  while (still < 6 && Date.now() < deadline) {
    await tick();
    await new Promise((resolve) => setTimeout(resolve, 5));
    flushSync();
    const active = document.activeElement;
    still = active === previous ? still + 1 : 0;
    previous = active;
  }
}

function press(element: Element, key: string): void {
  element.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }));
  flushSync();
}

async function type(key: string): Promise<void> {
  press(document.activeElement ?? document.body, key);
  await settle();
}

function mountMenu(props: Record<string, unknown> = {}): HTMLButtonElement {
  const before = document.createElement("button");
  document.body.append(before);
  const root = render(DropdownMenu, {
    triggerLabel: "Account",
    contentLabel: "Account actions",
    items: ITEMS,
    ...props,
  });
  const after = document.createElement("button");
  after.dataset.after = "";
  document.body.append(after);
  return root.querySelector<HTMLButtonElement>("[data-dropdown-menu-trigger]")!;
}

const surface = (): HTMLElement | null => document.querySelector("[data-dropdown-menu-surface]");
const rows = (): HTMLElement[] => [
  ...document.querySelectorAll<HTMLElement>('[role="menuitem"]'),
];
const labels = (): string[] =>
  rows().map((row) => row.firstElementChild?.textContent?.trim() ?? row.textContent?.trim() ?? "");
const focused = (): string => {
  const active = document.activeElement;
  return active?.firstElementChild?.textContent?.trim() ?? active?.textContent?.trim() ?? "";
};

async function open(trigger: HTMLElement, key = "ArrowDown"): Promise<void> {
  trigger.focus();
  press(trigger, key);
  await settle();
  expect(surface(), "the menu did not open").not.toBeNull();
  expect(document.activeElement?.getAttribute("role"), "no row took focus").toBe("menuitem");
}

describe("the Base DropdownMenu trigger", () => {
  it("is a canonical Button that identifies itself as a closed menu button", () => {
    const trigger = mountMenu();
    expect(trigger.tagName).toBe("BUTTON");
    expect(trigger.textContent).toContain("Account");
    expect(trigger.getAttribute("aria-haspopup")).toBe("menu");
    expect(trigger.getAttribute("aria-expanded")).toBe("false");
    expect(classes(trigger).has("inline-flex")).toBe(true);
    expect(classes(trigger).has("border-control-edge")).toBe(true);
  });

  it("renders no menu rows before it opens", () => {
    mountMenu();
    expect(surface()).toBeNull();
    expect(rows()).toEqual([]);
  });

  it("points aria-controls at the menu element that exists", async () => {
    const trigger = mountMenu();
    await open(trigger);
    const id = trigger.getAttribute("aria-controls");
    expect(id).toBeTruthy();
    expect(document.getElementById(id!)).toBe(surface());
  });
});

describe("the Base DropdownMenu structure", () => {
  it("leads mixed rows with a decorative DS Icon while keeping their labels aligned", async () => {
    const trigger = mountMenu({
      items: [
        { id: "profile", label: "Profile", icon: TestGlyph },
        { id: "settings", label: "Settings" },
      ],
    });
    await open(trigger);

    const [profile, settings] = rows();
    const icon = profile!.querySelector<SVGElement>("[data-icon]")!;
    const iconSlots = rows().map((row) => row.querySelector("[data-menu-item-icon]"));

    expect(icon.getAttribute("aria-hidden")).toBe("true");
    expect(icon.getAttribute("width")).toBe("var(--reddb-spatial-icon-size-md)");
    expect(profile!.getAttribute("aria-label")).toBeNull();
    expect(profile!.textContent?.trim()).toBe("Profile");
    expect(settings!.textContent?.trim()).toBe("Settings");
    expect(iconSlots.every(Boolean)).toBe(true);
    expect(classes(iconSlots[0]!).has("size-[var(--reddb-spatial-icon-size-md)]")).toBe(true);
    expect(classes(iconSlots[1]!)).toEqual(classes(iconSlots[0]!));
  });

  it("is a named vertical menu of rows in source order", async () => {
    const trigger = mountMenu();
    await open(trigger);
    expect(surface()?.getAttribute("role")).toBe("menu");
    expect(surface()?.getAttribute("aria-orientation")).toBe("vertical");
    expect(surface()?.getAttribute("aria-label")).toBe("Account actions");
    expect(trigger.getAttribute("aria-expanded")).toBe("true");
    expect(labels()).toEqual(["Profile", "Settings", "Archive", "Delete"]);
    expect(surface()!.querySelector("[data-menu-item-icon]")).toBeNull();
  });

  it("names each group with a presentational heading and separates runs", async () => {
    const trigger = mountMenu();
    await open(trigger);
    const separator = surface()!.querySelector("[data-dropdown-menu-separator]")!;
    expect(separator.getAttribute("role")).toBe("separator");
    expect(separator.getAttribute("aria-orientation")).toBe("horizontal");
    const heading = surface()!.querySelector("[data-dropdown-menu-group-heading]")!;
    expect(heading.textContent?.trim()).toBe("Danger zone");
    expect(heading.getAttribute("role")).toBe("presentation");
    const named = surface()!.querySelector(`[role="group"][aria-labelledby="${heading.id}"]`);
    expect(named).not.toBeNull();
    expect(named!.contains(heading)).toBe(true);
    expect(surface()!.querySelectorAll('[role="group"]')).toHaveLength(2);
  });

  it("keeps a disabled row present and announced as unavailable", async () => {
    const trigger = mountMenu();
    await open(trigger);
    const disabled = rows().find((row) => row.textContent?.trim() === "Delete")!;
    expect(disabled.getAttribute("aria-disabled")).toBe("true");
    expect(disabled.hasAttribute("data-disabled")).toBe(true);
  });

  it("renders navigation rows as links and action rows as non-links", async () => {
    const trigger = mountMenu();
    await open(trigger);
    const [profile, settings] = rows();
    expect(settings!.tagName).toBe("A");
    expect(settings!.getAttribute("href")).toBe("/settings");
    expect(settings!.getAttribute("role")).toBe("menuitem");
    expect(profile!.tagName).toBe("DIV");
    expect(profile!.querySelectorAll("kbd").length).toBeGreaterThan(0);
  });
});

describe("the Base DropdownMenu keyboard contract", () => {
  it("opens on ArrowDown with the first enabled row focused", async () => {
    const trigger = mountMenu();
    await open(trigger);
    expect(focused()).toContain("Profile");
  });

  it("opens on Enter", async () => {
    const trigger = mountMenu();
    await open(trigger, "Enter");
    expect(surface()).not.toBeNull();
  });

  it("walks enabled rows with arrows and wraps in both directions", async () => {
    const trigger = mountMenu();
    await open(trigger);
    await type("ArrowDown");
    expect(focused()).toBe("Settings");
    await type("ArrowDown");
    expect(focused()).toBe("Archive");
    await type("ArrowDown");
    expect(focused()).toContain("Profile");
    await type("ArrowUp");
    expect(focused()).toBe("Archive");
  });

  it("jumps to the enabled ends with Home and End", async () => {
    const trigger = mountMenu();
    await open(trigger);
    await type("End");
    expect(focused()).toBe("Archive");
    await type("Home");
    expect(focused()).toContain("Profile");
  });

  it("finds an enabled row by its first letter", async () => {
    const trigger = mountMenu();
    await open(trigger);
    await type("a");
    expect(focused()).toBe("Archive");
  });

  it("accumulates letters for multi-character typeahead", async () => {
    const trigger = mountMenu({
      items: [
        { id: "settings", label: "Settings" },
        { id: "sign-out", label: "Sign out" },
        { id: "profile", label: "Profile" },
      ],
    });
    await open(trigger);
    await type("s");
    expect(focused()).toBe("Sign out");
    await type("e");
    expect(focused()).toBe("Settings");
  });

  it("chooses the focused action with Enter and closes", async () => {
    const chosen: string[] = [];
    const trigger = mountMenu({
      items: [
        { id: "rename", label: "Rename", onselect: () => chosen.push("rename") },
        { id: "duplicate", label: "Duplicate", onselect: () => chosen.push("duplicate") },
      ],
    });
    await open(trigger);
    await type("ArrowDown");
    await type("Enter");
    expect(chosen).toEqual(["duplicate"]);
    expect(surface()).toBeNull();
  });

  it("closes on Escape and restores trigger focus", async () => {
    const trigger = mountMenu();
    await open(trigger);
    await type("Escape");
    expect(surface()).toBeNull();
    expect(document.activeElement).toBe(trigger);
    expect(trigger.getAttribute("aria-expanded")).toBe("false");
  });

  it("closes on Tab and advances to the next page control", async () => {
    const trigger = mountMenu();
    await open(trigger);
    await type("Tab");
    expect(surface()).toBeNull();
    expect((document.activeElement as HTMLElement | null)?.dataset.after).toBe("");
  });
});

describe("the Base DropdownMenu appearance", () => {
  it("composes Popover appearance on Bits' own menu element", async () => {
    const trigger = mountMenu({ class: "max-w-lg" });
    await open(trigger);
    for (const tokenClass of [
      "border-elevation-overlay-border",
      "bg-elevation-overlay-surface",
      "shadow-elevation-overlay",
      "motion-reduce:transition-none",
    ]) {
      expect(classesOf(popoverAppearance()).has(tokenClass)).toBe(true);
      expect(classes(surface()!).has(tokenClass)).toBe(true);
    }
    expect(classes(surface()!).has("max-w-lg")).toBe(true);
    expect(classes(surface()!).has("rounded-lg")).toBe(true);
  });

  it("moves trigger and rows to the requested density size", async () => {
    const trigger = mountMenu({ size: "sm" });
    await open(trigger);
    expect(classes(trigger).has("min-h-[var(--reddb-spatial-control-height-sm)]")).toBe(true);
    expect(classes(rows()[0]!).has("h-[var(--reddb-spatial-control-height-sm)]")).toBe(true);
  });

  it("keeps the requested Button variant", () => {
    expect(classes(mountMenu({ variant: "primary" })).has("bg-primary")).toBe(true);
  });

  it("keeps positioning configuration on the surface contract", async () => {
    const trigger = mountMenu({ collisionPadding: 12 });
    await open(trigger);
    expect(surface()!.dataset.collisionPadding).toBe("12");
  });
});

describe("the Base DropdownMenu grouping contract", () => {
  it("folds consecutive rows into one unlabelled group", () => {
    const groups = dropdownMenuGroups([
      { id: "a", label: "A" },
      { id: "b", label: "B" },
    ]);
    expect(groups).toHaveLength(1);
    expect(groups[0]!.heading).toBeUndefined();
    expect(groups[0]!.items.map((item) => item.id)).toEqual(["a", "b"]);
  });

  it("starts a new run at every explicit group and after it", () => {
    const groups = dropdownMenuGroups([
      { id: "a", label: "A" },
      { heading: "Titled", items: [{ id: "b", label: "B" }] },
      { id: "c", label: "C" },
    ]);
    expect(groups.map((group) => group.heading)).toEqual([undefined, "Titled", undefined]);
    expect(groups.map((group) => group.items.length)).toEqual([1, 1, 1]);
  });

  it("drops empty groups", () => {
    expect(dropdownMenuGroups([{ heading: "Nothing", items: [] }])).toEqual([]);
    expect(dropdownMenuGroups([])).toEqual([]);
  });

  it("flattens grouped entries back to source-order rows", () => {
    expect(dropdownMenuItems(ITEMS).map((item) => item.id)).toEqual([
      "profile",
      "settings",
      "archive",
      "delete",
    ]);
  });

  it("draws no separator for one run", async () => {
    const trigger = mountMenu({ items: [{ id: "a", label: "A" }, { id: "b", label: "B" }] });
    await open(trigger);
    expect(surface()!.querySelectorAll("[data-dropdown-menu-separator]")).toHaveLength(0);
  });
});
