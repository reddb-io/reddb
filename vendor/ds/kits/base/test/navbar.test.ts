import { createRawSnippet, flushSync, tick, type Snippet } from "svelte";
import { describe, expect, it } from "vitest";
import {
  Navbar,
  arrangements,
  NAVBAR_ALIGNMENTS,
  NAVBAR_COLLAPSE,
  navbar as navbarAppearance,
  type NavbarLink,
} from "@reddb-io/design-system/base";
import NavbarMasthead from "./fixtures/NavbarMasthead.svelte";
import { labelInName } from "./label-in-name";
import { classes, render, rendered } from "./mount";

function raw(html: string): Snippet {
  return createRawSnippet(() => ({ render: () => html }));
}

const BRAND = raw('<a href="/" aria-label="reddb.io home" data-brand>reddb.io</a>');
const ACTIONS = raw('<button type="button" data-action>Sign in</button>');
const LINKS: readonly NavbarLink[] = [
  { id: "nodes", label: "Nodes", href: "/nodes", active: true },
  { id: "queries", label: "Queries", href: "/queries" },
  { id: "docs", label: "Docs", href: "/docs", disabled: true },
];

function mountNavbar(props: Record<string, unknown> = {}): HTMLElement {
  return rendered(
    render(Navbar, {
      links: LINKS,
      brand: BRAND,
      actions: ACTIONS,
      collapse: "expanded",
      ...props,
    }),
  );
}

const rail = (nav: HTMLElement): HTMLElement | null =>
  nav.querySelector('[data-navbar-arrangement="rail"]');
const compact = (nav: HTMLElement): HTMLElement | null =>
  nav.querySelector('[data-navbar-arrangement="compact"]');
const panel = (nav: HTMLElement): HTMLElement | null => nav.querySelector("[data-navbar-panel]");
const toggle = (nav: HTMLElement): HTMLButtonElement =>
  nav.querySelector<HTMLButtonElement>("[data-navbar-toggle]")!;
const region = (where: Element, name: string): HTMLElement | null =>
  where.querySelector(`[data-navbar-region="${name}"]`);
const linksIn = (where: Element): HTMLElement[] => [
  ...where.querySelectorAll<HTMLElement>("[data-navbar-link]"),
];
const labelsIn = (where: Element): string[] =>
  linksIn(where).map((link) => link.textContent?.trim() ?? "");
const folded = (element: HTMLElement): boolean => element.hidden && classes(element).has("hidden");

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

function click(element: HTMLElement): void {
  element.click();
  flushSync();
}

function press(element: Element, key: string): void {
  element.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }));
  flushSync();
}

async function type(key: string): Promise<void> {
  press(document.activeElement ?? document.body, key);
  await settle();
}

const menu = (): HTMLElement | null => document.querySelector('[role="menu"]');
const focused = (): string =>
  document.activeElement?.firstElementChild?.textContent?.trim() ??
  document.activeElement?.textContent?.trim() ??
  "";

describe("the Base Navbar composes a masthead", () => {
  it("orders the brand, links, and actions regions", () => {
    const bar = rail(mountNavbar())!;
    expect(region(bar, "brand")!.querySelector("[data-brand]")).not.toBeNull();
    expect(region(bar, "actions")!.querySelector("[data-action]")).not.toBeNull();
    expect([...bar.children].map((child) => child.getAttribute("data-navbar-region"))).toEqual([
      "brand",
      "links",
      "actions",
    ]);
  });

  it("renders links in order with native current-page semantics", () => {
    const entries = linksIn(rail(mountNavbar())!);
    expect(entries.map((entry) => entry.textContent?.trim())).toEqual(["Nodes", "Queries", "Docs"]);
    expect(entries[0]!.tagName).toBe("A");
    expect(entries[0]!.getAttribute("href")).toBe("/nodes");
    expect(entries[0]!.getAttribute("aria-current")).toBe("page");
    expect(entries[1]!.hasAttribute("aria-current")).toBe(false);
  });

  it("preserves a caller's explicit aria-current token", () => {
    const nav = mountNavbar({
      links: [{ id: "checkout", label: "Checkout", href: "/checkout", current: "step" }],
    });
    expect(linksIn(rail(nav)!)[0]!.getAttribute("aria-current")).toBe("step");
  });

  it("renders an entry without a destination as a Button action", () => {
    const chosen: string[] = [];
    const nav = mountNavbar({
      links: [{ id: "search", label: "Search", onselect: () => chosen.push("search") }],
    });
    const action = linksIn(rail(nav)!)[0]!;
    expect(action.tagName).toBe("BUTTON");
    click(action);
    expect(chosen).toEqual(["search"]);
  });

  it("keeps a disabled destination visible but unreachable", () => {
    const docs = linksIn(rail(mountNavbar())!).find((link) => link.dataset.navbarLink === "docs")!;
    expect(docs.getAttribute("aria-disabled")).toBe("true");
    expect(docs.hasAttribute("href")).toBe(false);
  });

  it("omits an empty link list while leaving empty content regions", () => {
    const bar = rail(mountNavbar({ links: [], brand: undefined, actions: undefined }))!;
    expect(linksIn(bar)).toEqual([]);
    expect(bar.querySelector("ul")).toBeNull();
    expect(region(bar, "brand")!.textContent?.trim()).toBe("");
    expect(region(bar, "actions")!.textContent?.trim()).toBe("");
  });

  it("names the navigation landmark", () => {
    expect(mountNavbar().tagName).toBe("NAV");
    expect(mountNavbar().getAttribute("aria-label")).toBe("Main");
    expect(mountNavbar({ label: "Product" }).getAttribute("aria-label")).toBe("Product");
  });
});

describe("the Base Navbar alignments", () => {
  it("offers exactly start and center", () => {
    expect([...NAVBAR_ALIGNMENTS].sort()).toEqual(["center", "start"]);
    for (const align of NAVBAR_ALIGNMENTS) expect(mountNavbar({ align }).dataset.navbarAlign).toBe(align);
  });

  it("lets start-aligned links grow and pushes actions to the end", () => {
    const bar = rail(mountNavbar({ align: "start" }))!;
    expect(classes(region(bar, "links")!).has("grow")).toBe(true);
    expect(classes(region(bar, "brand")!).has("shrink-0")).toBe(true);
    expect(classes(region(bar, "actions")!).has("ms-auto")).toBe(true);
  });

  it("gives centered links equal growing regions on both sides", () => {
    const bar = rail(mountNavbar({ align: "center" }))!;
    for (const name of ["brand", "actions"]) {
      expect(classes(region(bar, name)!).has("grow")).toBe(true);
      expect(classes(region(bar, name)!).has("basis-0")).toBe(true);
    }
    expect(classes(region(bar, "links")!).has("shrink-0")).toBe(true);
    expect(classes(region(bar, "links")!).has("justify-center")).toBe(true);
  });
});

describe("the Base Navbar collapse arrangements", () => {
  it("renders both arrangements responsively and one in pinned modes", () => {
    expect(arrangements("responsive")).toEqual({ rail: true, compact: true });
    expect(arrangements("expanded")).toEqual({ rail: true, compact: false });
    expect(arrangements("collapsed")).toEqual({ rail: false, compact: true });
  });

  it("defines no collapse mode that renders nothing", () => {
    expect([...NAVBAR_COLLAPSE].sort()).toEqual(["collapsed", "expanded", "responsive"]);
    for (const collapse of NAVBAR_COLLAPSE) {
      const shown = arrangements(collapse);
      expect(shown.rail || shown.compact).toBe(true);
    }
  });

  it("puts exactly the selected arrangements in the document", () => {
    for (const collapse of NAVBAR_COLLAPSE) {
      const nav = mountNavbar({ collapse });
      const shown = arrangements(collapse);
      expect(rail(nav) !== null, `${collapse} rail`).toBe(shown.rail);
      expect(compact(nav) !== null, `${collapse} compact`).toBe(shown.compact);
      expect(panel(nav) !== null, `${collapse} panel`).toBe(shown.compact);
    }
  });

  it("leaves the responsive choice to CSS on its own container width, with equivalent links", () => {
    // Container-first (ADR 0021): the Navbar collapses on its own width, from
    // the DS's `3xl` container step, never on the window's.
    const nav = mountNavbar({ collapse: "responsive" });
    expect(classes(nav).has("@container/navbar")).toBe(true);
    expect(classes(rail(nav)!).has("hidden")).toBe(true);
    expect(classes(rail(nav)!).has("@3xl/navbar:flex")).toBe(true);
    expect(classes(compact(nav)!).has("flex")).toBe(true);
    expect(classes(compact(nav)!).has("@3xl/navbar:hidden")).toBe(true);
    expect(classes(panel(nav)!).has("@3xl/navbar:hidden")).toBe(true);
    for (const element of [nav, rail(nav)!, compact(nav)!, panel(nav)!]) {
      expect([...classes(element)].some((name) => /^(?:max-)?(?:sm|md|lg|xl|2xl):/.test(name))).toBe(false);
    }
    expect(labelsIn(panel(nav)!)).toEqual(labelsIn(rail(nav)!));
  });
});

describe("the Base Navbar collapsed disclosure", () => {
  const mountCollapsed = (props: Record<string, unknown> = {}): HTMLElement =>
    mountNavbar({ collapse: "collapsed", ...props });

  it("keeps the brand in the bar and folds links into the panel", () => {
    const nav = mountCollapsed();
    expect(region(compact(nav)!, "brand")!.querySelector("[data-brand]")).not.toBeNull();
    expect(linksIn(compact(nav)!)).toEqual([]);
    expect(labelsIn(panel(nav)!)).toEqual(["Nodes", "Queries", "Docs"]);
  });

  it("uses a real Button whose aria-controls target exists", () => {
    const nav = mountCollapsed();
    const button = toggle(nav);
    expect(button.tagName).toBe("BUTTON");
    expect(button.getAttribute("aria-expanded")).toBe("false");
    expect(button.getAttribute("aria-label")).toBe("Menu");
    const controls = button.getAttribute("aria-controls");
    expect(controls).toBeTruthy();
    expect(document.getElementById(controls!)).toBe(panel(nav));
  });

  it("keeps the closed panel out of the accessibility tree", () => {
    expect(folded(panel(mountCollapsed())!)).toBe(true);
  });

  it("opens and closes from the disclosure Button", () => {
    const nav = mountCollapsed();
    const button = toggle(nav);
    click(button);
    expect(button.getAttribute("aria-expanded")).toBe("true");
    expect(folded(panel(nav)!)).toBe(false);
    click(button);
    expect(button.getAttribute("aria-expanded")).toBe("false");
    expect(folded(panel(nav)!)).toBe(true);
  });

  it("carries the actions region into the open panel", () => {
    const nav = mountCollapsed();
    click(toggle(nav));
    expect(region(panel(nav)!, "actions")!.querySelector("[data-action]")).not.toBeNull();
  });

  it("closes on Escape and restores disclosure focus", async () => {
    const nav = mountCollapsed();
    const button = toggle(nav);
    button.focus();
    click(button);
    press(button, "Escape");
    await settle();
    expect(folded(panel(nav)!)).toBe(true);
    expect(button.getAttribute("aria-expanded")).toBe("false");
    expect(document.activeElement).toBe(button);
  });

  it("closes after a panel link is chosen", () => {
    const chosen: string[] = [];
    const nav = mountCollapsed({
      links: [{ id: "nodes", label: "Nodes", href: "#nodes", onselect: () => chosen.push("nodes") }],
    });
    click(toggle(nav));
    click(linksIn(panel(nav)!)[0]!);
    expect(chosen).toEqual(["nodes"]);
    expect(folded(panel(nav)!)).toBe(true);
  });

  it("does not steal Escape while already closed", async () => {
    const nav = mountCollapsed();
    const button = toggle(nav);
    button.focus();
    press(button, "Escape");
    await settle();
    expect(document.activeElement).toBe(button);
    expect(folded(panel(nav)!)).toBe(true);
  });
});

describe("menus composed into Base Navbar actions", () => {
  it("renders real menu Buttons including a custom avatar trigger", () => {
    const nav = rendered(render(NavbarMasthead, { collapse: "expanded" }));
    const triggers = region(rail(nav)!, "actions")!.querySelectorAll("[data-dropdown-menu-trigger]");
    expect(triggers).toHaveLength(2);
    for (const trigger of triggers) {
      expect(trigger.getAttribute("aria-haspopup")).toBe("menu");
      expect(trigger.getAttribute("aria-expanded")).toBe("false");
    }
    expect(nav.querySelector("[data-avatar]")).not.toBeNull();
    // The avatar's initials lead its name, so speech input can say what it shows (WCAG 2.5.3).
    expect(triggers[1]!.getAttribute("aria-label")).toBe("RD account");
    for (const trigger of triggers) expect(labelInName(trigger)).toBe(true);
  });

  it("opens, walks, and closes an expanded masthead menu from the keyboard", async () => {
    const nav = rendered(render(NavbarMasthead, { collapse: "expanded" }));
    const trigger = region(rail(nav)!, "actions")!.querySelector<HTMLElement>("[data-dropdown-menu-trigger]")!;
    trigger.focus();
    press(trigger, "ArrowDown");
    await settle();
    expect(menu()).not.toBeNull();
    expect(focused()).toBe("Invite accepted");
    await type("ArrowDown");
    expect(focused()).toBe("Backup finished");
    await type("Escape");
    expect(menu()).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  it("lets a portaled menu close before its collapsed parent panel", async () => {
    const nav = rendered(render(NavbarMasthead, { collapse: "collapsed" }));
    const button = toggle(nav);
    click(button);
    const trigger = panel(nav)!.querySelector<HTMLElement>("[data-dropdown-menu-trigger]")!;
    trigger.focus();
    press(trigger, "ArrowDown");
    await settle();
    expect(menu()).not.toBeNull();
    await type("Escape");
    expect(menu()).toBeNull();
    expect(folded(panel(nav)!)).toBe(false);
    await type("Escape");
    expect(folded(panel(nav)!)).toBe(true);
    expect(document.activeElement).toBe(button);
  });

  it("chooses a menu action in the panel without collapsing navigation", async () => {
    const chosen: string[] = [];
    const nav = rendered(
      render(NavbarMasthead, {
        collapse: "collapsed",
        account: [
          { id: "profile", label: "Profile", onselect: () => chosen.push("profile") },
          { id: "sign-out", label: "Sign out", onselect: () => chosen.push("sign-out") },
        ],
      }),
    );
    click(toggle(nav));
    const triggers = panel(nav)!.querySelectorAll<HTMLElement>("[data-dropdown-menu-trigger]");
    const account = triggers[triggers.length - 1]!;
    account.focus();
    press(account, "Enter");
    await settle();
    await type("ArrowDown");
    expect(focused()).toBe("Sign out");
    await type("Enter");
    expect(chosen).toEqual(["sign-out"]);
    expect(menu()).toBeNull();
    expect(folded(panel(nav)!)).toBe(false);
  });
});

describe("the Base Navbar appearance", () => {
  it("wears the Theme masthead surface with a quiet boundary", () => {
    const worn = classes(mountNavbar());
    expect(worn.has("bg-navigation-surface")).toBe(true);
    expect(worn.has("border-muted")).toBe(true);
    expect(worn.has("font-sans")).toBe(true);
  });

  it("uses Density axis insets and gaps", () => {
    const worn = classes(rail(mountNavbar())!);
    expect(worn.has("min-h-[var(--reddb-spatial-control-height-lg)]")).toBe(true);
    expect(worn.has("px-[var(--reddb-spatial-inset-md)]")).toBe(true);
    expect(worn.has("gap-[var(--reddb-spatial-gap-lg)]")).toBe(true);
  });

  it("uses a canonical Button for the collapsed toggle", () => {
    const worn = classes(toggle(mountNavbar({ collapse: "collapsed" })));
    expect(worn.has("inline-flex")).toBe(true);
    expect(worn.has("min-h-[var(--reddb-spatial-control-height-md)]")).toBe(true);
  });

  it("merges caller classes over its canonical appearance", () => {
    const nav = mountNavbar({ class: "sticky" });
    expect(classes(nav)).toEqual(new Set(navbarAppearance().root({ class: "sticky" }).split(/\s+/)));
  });
});

describe("the Navbar current link", () => {
  it("wears a neutral surface and a 2px Brand underline on the rail, a start bar in the panel", () => {
    const nav = mountNavbar({ collapse: "responsive" });
    const railCurrent = rail(nav)!.querySelector<HTMLElement>('[data-navbar-link="nodes"]')!;
    const railOther = rail(nav)!.querySelector<HTMLElement>('[data-navbar-link="queries"]')!;
    const panelCurrent = nav
      .querySelector<HTMLElement>("[data-navbar-panel]")!
      .querySelector<HTMLElement>('[data-navbar-link="nodes"]')!;

    expect(railCurrent.getAttribute("aria-current")).toBe("page");
    for (const [element, edge] of [
      [railCurrent, "border-b-2"],
      [railOther, "border-b-2"],
      [panelCurrent, "border-s-2"],
    ] as const) {
      const names = classes(element);
      expect(names.has(edge)).toBe(true);
      expect(names.has("border-transparent")).toBe(true);
      expect(names.has("aria-[current]:border-foreground")).toBe(true);
      expect([...names].some((name) => /border(?:-[bstexy])?-primary/.test(name))).toBe(false);
      expect(names.has("aria-[current]:bg-foreground/10")).toBe(true);
      expect(names.has("aria-[current]:font-medium")).toBe(true);
      expect(names.has("aria-[current]:text-foreground")).toBe(true);
      expect([...names].some((name) => /(?:^|:)bg-primary(?:\/|$)|text-primary/.test(name))).toBe(false);
    }
  });
});
