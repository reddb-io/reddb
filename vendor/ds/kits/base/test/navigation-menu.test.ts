import { flushSync, tick } from "svelte";
import { describe, expect, it } from "vitest";
import {
  NavigationMenu,
  popover as popoverAppearance,
  type NavigationMenuEntry,
} from "@reddb-io/design-system/base";
import { classes, classesOf, render } from "./mount";

const ITEMS: readonly NavigationMenuEntry[] = [
  { id: "overview", label: "Overview", href: "/overview", active: true },
  {
    id: "resources",
    label: "Resources",
    links: [
      { id: "docs", label: "Documentation", href: "/docs", description: "Read the guides" },
      { id: "support", label: "Support", href: "/support" },
    ],
  },
  { id: "pricing", label: "Pricing", href: "/pricing" },
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

describe("the Base NavigationMenu", () => {
  it("renders native links in a named navigation and roves the top-level focus", async () => {
    const root = render(NavigationMenu, { label: "Primary", items: ITEMS });
    const nav = root.querySelector<HTMLElement>("[data-navigation-menu-root]")!;
    const controls = [...root.querySelectorAll<HTMLElement>("[data-navigation-menu-control]")];

    expect(nav.tagName).toBe("NAV");
    expect(nav.getAttribute("aria-label")).toBe("Primary");
    expect(controls[0]!.tagName).toBe("A");
    expect(controls[0]!.getAttribute("href")).toBe("/overview");
    expect(controls[0]!.getAttribute("aria-current")).toBe("page");

    controls[0]!.focus();
    press(controls[0]!, "ArrowRight");
    await settle();
    expect(document.activeElement).toBe(controls[1]);
  });

  it("dismisses an opened navigation surface with Escape", async () => {
    const root = render(NavigationMenu, { label: "Primary", items: ITEMS, delayDuration: 0 });
    const trigger = root.querySelector<HTMLButtonElement>("[data-navigation-menu-trigger]")!;
    trigger.focus();
    trigger.click();
    await settle();

    const content = document.querySelector<HTMLElement>("[data-navigation-menu-surface]")!;
    expect(content).not.toBeNull();
    press(trigger, "Escape");
    await settle();

    expect(document.querySelector("[data-navigation-menu-surface]")).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  it("composes the canonical Popover surface and live Density roles", async () => {
    const root = render(NavigationMenu, {
      label: "Primary",
      items: ITEMS,
      value: "resources",
      delayDuration: 0,
      size: "lg",
      class: "max-w-xl",
    });
    await settle();
    const content = document.querySelector<HTMLElement>("[data-navigation-menu-surface]")!;

    // The contract is "this surface IS the canonical Popover surface", so the
    // expectation is read from the Popover rather than restated as a list of
    // class names. A list goes stale the moment the Foundation renames a role —
    // which is exactly how this assertion came to pin `bg-background` after
    // elevation moved the Popover onto its overlay level.
    //
    // Composition is measured on a surface the caller has not overridden;
    // the override itself is the assertion below.
    render(NavigationMenu, { label: "Plain", items: ITEMS, value: "resources", delayDuration: 0 });
    await settle();
    const plain = [...document.querySelectorAll<HTMLElement>("[data-navigation-menu-surface]")].at(-1)!;
    for (const tokenClass of classesOf(popoverAppearance())) {
      expect(classes(plain).has(tokenClass), `composes ${tokenClass}`).toBe(true);
    }
    expect(classes(content).has("max-w-xl")).toBe(true);
    expect(classes(root.querySelector("[data-navigation-menu-control]")!).has("h-[var(--reddb-spatial-control-height-lg)]")).toBe(true);
    expect(content.querySelectorAll("a")).toHaveLength(2);
  });

  it("keeps a closed section's destinations in the document without a second tab stop", async () => {
    const root = render(NavigationMenu, { label: "Primary", items: ITEMS, delayDuration: 0 });
    const closed = root.querySelector<HTMLElement>('[data-navigation-menu-closed-links="resources"]')!;

    // Crawlers and no-script readers find every destination while the section is closed …
    expect(closed).not.toBeNull();
    expect(closed.hidden).toBe(true);
    expect([...closed.querySelectorAll("a")].map((link) => link.getAttribute("href")))
      .toEqual(["/docs", "/support"]);
    expect(closed.textContent).toContain("Read the guides");
    expect(document.querySelector("[data-navigation-menu-surface]")).toBeNull();

    // … and the real surface takes over on open, so no link is ever duplicated.
    root.querySelector<HTMLButtonElement>("[data-navigation-menu-trigger]")!.click();
    await settle();
    expect(document.querySelector("[data-navigation-menu-surface]")).not.toBeNull();
    expect(root.querySelector("[data-navigation-menu-closed-links]")).toBeNull();
    expect(document.querySelectorAll('a[href="/docs"]')).toHaveLength(1);
  });
});
