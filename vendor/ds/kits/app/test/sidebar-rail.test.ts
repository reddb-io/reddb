import { createRawSnippet, flushSync, tick } from "svelte";
import { describe, expect, it, vi } from "vitest";
import {
  SidebarLayout,
  SidebarRail,
  sidebarLayout,
  sidebarRail,
  type SidebarRailItem,
} from "../src/index";
import { labelInName } from "./label-in-name";
import { classes, classesOf, render, rendered } from "./mount";

const snippet = (html: string) => createRawSnippet(() => ({ render: () => html }));

const ITEMS: readonly SidebarRailItem[] = [
  { id: "acme", label: "Acme workspace", fallback: "AC" },
  { id: "orbit", label: "Orbit workspace", fallback: "OR" },
  { id: "add", label: "Add workspace", fallback: "+" },
];

async function settle(): Promise<void> {
  await tick();
  flushSync();
}

describe("SidebarRail", () => {
  it("names every icon-only control and roves focus with vertical arrow keys", async () => {
    const root = render(SidebarRail, {
      label: "Organizations",
      items: ITEMS,
      currentId: "acme",
    });
    const rail = root.querySelector<HTMLElement>("nav[data-sidebar-rail]")!;
    const controls = [...rail.querySelectorAll<HTMLButtonElement>("[data-sidebar-rail-item]")];

    expect(rail.getAttribute("aria-label")).toBe("Organizations");
    expect(controls.map((control) => control.getAttribute("aria-label"))).toEqual([
      "AC, Acme workspace",
      "OR, Orbit workspace",
      "Add workspace",
    ]);
    expect(controls.map((control) => control.tabIndex)).toEqual([0, -1, -1]);

    controls[0]!.focus();
    controls[0]!.dispatchEvent(
      new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true, cancelable: true }),
    );
    await settle();

    expect(document.activeElement).toBe(controls[1]);
    expect(controls.map((control) => control.tabIndex)).toEqual([-1, 0, -1]);
    expect(document.body.querySelector('[role="tooltip"]')?.textContent).toBe("Orbit workspace");
  });

  it("keeps native destinations in the same roving focus order as section buttons", async () => {
    const root=render(SidebarRail,{label:"Primary",currentId:"home",items:[{id:"home",label:"Home",href:"/home"},{id:"users",label:"Users"},{id:"reports",label:"Reports",href:"/reports",disabled:true}]});
    const entries=[...root.querySelectorAll<HTMLElement>("[data-sidebar-rail-item]")];
    expect(entries.map(entry=>entry.tagName)).toEqual(["A","BUTTON","A"]);
    expect(entries.map(entry=>entry.tabIndex)).toEqual([0,-1,-1]);
    entries[0]!.focus();entries[0]!.dispatchEvent(new KeyboardEvent("keydown",{key:"ArrowDown",bubbles:true,cancelable:true}));await settle();
    expect(document.activeElement).toBe(entries[1]);
    expect(entries[2]!.hasAttribute("href")).toBe(false);
  });

  it("leads a tile's name with the initials it shows (WCAG 2.5.3, Label in Name)", () => {
    const root = render(SidebarRail, {
      label: "Organizations",
      items: [
        { id: "reddb", label: "RedDB", fallback: "RD" },
        { id: "derived", label: "Northstar" },
        { id: "held", label: "QA lab", fallback: "QA" },
        { id: "add", label: "Add organization", fallback: "+" },
      ],
    });
    const controls = [...root.querySelectorAll<HTMLButtonElement>("[data-sidebar-rail-item]")];

    expect(controls.map((control) => control.getAttribute("aria-label"))).toEqual([
      "RD, RedDB",
      "NO, Northstar",
      "QA lab",
      "Add organization",
    ]);
    for (const control of controls) expect(labelInName(control), control.getAttribute("aria-label")!).toBe(true);
    // The tile still shows only the initials; the label stays the Tooltip content.
    expect(controls[0]!.textContent?.trim()).toBe("RD");
  });

  it("shows labels below faces and separates groups without adding focus stops", async () => {
    const root = render(SidebarRail, { label: "Areas", showLabels: true, items: [
      { id: "users", label: "Users" },
      { id: "workspaces", label: "Workspaces", separatorBefore: true, disabled: true },
      { id: "settings", label: "Settings", separatorBefore: true },
    ] });
    expect(root.querySelectorAll("[data-sidebar-rail-label]")).toHaveLength(3);
    expect(root.querySelectorAll('[role="separator"]')).toHaveLength(2);
    const buttons = [...root.querySelectorAll<HTMLButtonElement>("[data-sidebar-rail-item]")];
    expect(buttons.filter(button => !button.disabled && button.tabIndex === 0)).toHaveLength(1);
    buttons[0]!.focus();
    buttons[0]!.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true, cancelable: true }));
    await settle();
    expect(document.activeElement).toBe(buttons[2]);
    for (const button of buttons) expect(labelInName(button)).toBe(true);
  });

  it("emits the selected id without taking ownership of organization state", () => {
    const onselect = vi.fn();
    const root = render(SidebarRail, {
      label: "Organizations",
      items: ITEMS,
      currentId: "acme",
      onselect,
    });
    const controls = [...root.querySelectorAll<HTMLButtonElement>("[data-sidebar-rail-item]")];

    expect(controls[0]!.getAttribute("aria-pressed")).toBe("true");
    expect(controls[1]!.getAttribute("aria-pressed")).toBe("false");

    controls[1]!.click();
    flushSync();

    expect(onselect).toHaveBeenCalledOnce();
    expect(onselect).toHaveBeenCalledWith("orbit");
    expect(controls[0]!.getAttribute("aria-pressed")).toBe("true");
    expect(controls[1]!.getAttribute("aria-pressed")).toBe("false");
  });

  it("portals a middle-region tooltip beyond the rail overflow container", () => {
    const root = render(SidebarRail, {
      label: "Organizations",
      items: ITEMS,
      currentId: "acme",
    });
    const middle = root.querySelector<HTMLElement>('[data-sidebar-rail-region="middle"]')!;
    middle.querySelector<HTMLButtonElement>('[data-sidebar-rail-item="orbit"]')!.focus();
    flushSync();

    expect(middle.querySelector('[role="tooltip"]')).toBeNull();
    expect(document.body.querySelector<HTMLElement>('[role="tooltip"]')?.textContent).toBe(
      "Orbit workspace",
    );
  });

  it("keeps the selected tile's focus ring inside the scrolling middle region", () => {
    const root = render(SidebarRail, {
      label: "Organizations",
      items: ITEMS,
      currentId: "orbit",
    });
    const middle = root.querySelector<HTMLElement>('[data-sidebar-rail-region="middle"]')!;
    const selected = middle.querySelector<HTMLElement>(
      '[data-sidebar-rail-item="orbit"]',
    )!;

    expect(classes(middle).has("overflow-y-auto")).toBe(true);
    expect(selected.getAttribute("aria-pressed")).toBe("true");
    expect(classes(selected).has("aria-pressed:border-foreground")).toBe(true);
    expect(classes(selected).has("aria-pressed:border-primary")).toBe(false);
    // The ink focus outline (ADR 0023), drawn inside the tile: an outline
    // outside it would be clipped by the scroll region on both sides.
    expect(classes(selected).has("focus-visible:outline-focus")).toBe(true);
    expect(classes(selected).has("focus-visible:-outline-offset-2")).toBe(true);
  });

  it("marks the selected tile with a neutral surface and a start-edge Brand bar, never a red fill or outline", () => {
    const root = render(SidebarRail, {
      label: "Organizations",
      items: ITEMS,
      currentId: "orbit",
    });
    for (const item of root.querySelectorAll<HTMLElement>("[data-sidebar-rail-item]")) {
      const itemClasses = classes(item);
      // Every tile carries the same 2px start edge, so selection moves nothing.
      expect(itemClasses.has("border-s-2")).toBe(true);
      expect(itemClasses.has("border-transparent")).toBe(true);
      expect(itemClasses.has("aria-pressed:bg-foreground/10")).toBe(true);
      expect(itemClasses.has("aria-pressed:text-foreground")).toBe(true);
      expect([...itemClasses].some((name) => /(?:^|:)bg-primary(?:\/|$)/.test(name))).toBe(false);
      expect([...itemClasses].some((name) => /(?:^|:)(?:ring|outline)-primary$/.test(name) && !name.startsWith("focus-visible:"))).toBe(false);
      expect(itemClasses.has("border")).toBe(false);
    }
  });

  it("keeps rail, tile, and gap dimensions live on Density roles", () => {
    const rail = rendered(
      render(SidebarRail, { label: "Organizations", items: ITEMS, currentId: "acme" }),
    );
    const list = rail.querySelector<HTMLElement>("[data-sidebar-rail-items]")!;
    const item = rail.querySelector<HTMLElement>("[data-sidebar-rail-item]")!;

    expect(classes(rail)).toEqual(classesOf(sidebarRail().root()));
    expect(
      classes(rail).has(
        "w-[calc(var(--reddb-spatial-control-height-md)+2*var(--reddb-spatial-inset-sm))]",
      ),
    ).toBe(true);
    expect(classes(list).has("gap-[var(--reddb-spatial-gap-md)]")).toBe(true);
    expect(classes(item).has("size-[var(--reddb-spatial-control-height-md)]")).toBe(true);
    expect(rail.hasAttribute("data-density")).toBe(false);
  });

  it("keeps explicit top, middle, and bottom regions in one navigation landmark", () => {
    const rail = rendered(
      render(SidebarRail, {
        label: "Application rail",
        items: ITEMS,
        currentId: "acme",
        top: snippet('<a href="/" aria-label="Home">Brand</a>'),
        bottom: snippet('<button type="button" aria-label="Account">JD</button>'),
      }),
    );

    expect([...rail.children].map((region) => region.getAttribute("data-sidebar-rail-region")))
      .toEqual(["top", "middle", "bottom"]);
    expect(rail.querySelector('[data-sidebar-rail-region="top"] a')?.getAttribute("href"))
      .toBe("/");
    expect(rail.querySelector('[data-sidebar-rail-region="bottom"] button')?.getAttribute("aria-label"))
      .toBe("Account");
  });
});

describe("the double-sidebar composition", () => {
  it.each([
    {
      railOpen: true,
      panelOpen: true,
      columns:
        "md:grid-cols-[auto_minmax(12rem,1fr)_minmax(0,3fr)]",
    },
    {
      railOpen: true,
      panelOpen: false,
      columns:
        "md:grid-cols-[auto_0_minmax(0,1fr)]",
    },
    {
      railOpen: false,
      panelOpen: true,
      columns: "md:grid-cols-[0_minmax(12rem,1fr)_minmax(0,3fr)]",
    },
    {
      railOpen: false,
      panelOpen: false,
      columns: "md:grid-cols-[0_0_minmax(0,1fr)]",
    },
  ])(
    "reflows content when rail open is $railOpen and panel open is $panelOpen",
    ({ railOpen, panelOpen, columns }) => {
      const layout = rendered(
        render(SidebarLayout, {
          rail: snippet('<nav aria-label="Organizations">Rail</nav>'),
          sidebar: snippet('<nav aria-label="Workspace">Panel</nav>'),
          railOpen,
          panelOpen,
          children: snippet("<h1>Dashboard</h1>"),
        }),
      );
      const rail = layout.querySelector<HTMLElement>("[data-sidebar-layout-region=rail]")!;

      expect(layout.getAttribute("data-rail-open")).toBe(String(railOpen));
      expect(layout.getAttribute("data-panel-open")).toBe(String(panelOpen));
      expect(classes(layout).has(columns)).toBe(true);
      expect(classes(layout).has("transition-[grid-template-columns]")).toBe(true);
      expect(classes(layout).has("motion-reduce:transition-none")).toBe(true);
      expect(classes(rail).has("md:w-0")).toBe(!railOpen);
    },
  );

  it("pairs rail and panel navigation while exposing the panel as a mobile drawer", () => {
    const layout = rendered(
      render(SidebarLayout, {
        rail: snippet('<nav aria-label="Organizations">Rail</nav>'),
        sidebar: snippet('<nav aria-label="Workspace">Panel</nav>'),
        panelOpen: true,
        children: snippet("<h1>Dashboard</h1>"),
      }),
    );
    const panel = layout.querySelector<HTMLElement>("[data-sidebar-layout-region=sidebar]")!;

    expect([...layout.querySelectorAll("nav")].map((nav) => nav.getAttribute("aria-label")))
      .toEqual(["Organizations", "Workspace"]);
    expect(layout.getAttribute("data-sidebar-layout-mode")).toBe("rail-panel");
    expect(layout.getAttribute("data-panel-open")).toBe("true");
    expect(classes(layout)).toEqual(
      classesOf(sidebarLayout({ side: "start", rail: true, panelOpen: true }).root()),
    );
    expect(classes(panel).has("max-md:fixed")).toBe(true);
    expect(panel.getAttribute("data-mobile-presentation")).toBe("drawer");
  });

  it("removes and restores the desktop panel track without motion dependence", () => {
    const layout = (panelOpen: boolean) =>
      rendered(
        render(SidebarLayout, {
          rail: snippet('<nav aria-label="Organizations">Rail</nav>'),
          sidebar: snippet('<nav aria-label="Workspace">Panel</nav>'),
          panelOpen,
          children: snippet("<h1>Dashboard</h1>"),
        }),
      );
    const closed = layout(false);
    const open = layout(true);
    const closedPanel = closed.querySelector<HTMLElement>(
      "[data-sidebar-layout-region=sidebar]",
    )!;

    expect(classes(closed).has("transition-[grid-template-columns]")).toBe(true);
    expect(classes(closed).has("motion-reduce:transition-none")).toBe(true);
    expect(
      classes(closed).has(
        "md:grid-cols-[auto_0_minmax(0,1fr)]",
      ),
    ).toBe(true);
    expect(classes(closedPanel).has("md:w-0")).toBe(true);
    expect(
      classes(open).has(
        "md:grid-cols-[auto_minmax(12rem,1fr)_minmax(0,3fr)]",
      ),
    ).toBe(true);
  });
});
