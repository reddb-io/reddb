import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import SidebarLayout from "../src/composites/SidebarLayout.svelte";
import { sidebarLayout } from "../src/composites/sidebar-layout.variants";
import ApplicationLayoutsContractFailures from "./fixtures/ApplicationLayoutsContractFailures.svelte";
import ApplicationLayoutsConsumer from "./fixtures/ApplicationLayoutsConsumer.svelte";
import { expectLayoutReadiness } from "./layout-readiness";
import { classes, classesOf, render, rendered } from "./mount";

const snippet = (html: string) => createRawSnippet(() => ({ render: () => html }));

describe("the deliberately failing Sidebar layout fixture", () => {
  it("diagnoses a sidebar arrangement that loses its main landmark", () => {
    const root = render(ApplicationLayoutsContractFailures, { failure: "sidebar-without-main" });

    expect(root.querySelector("aside")).not.toBeNull();
    expect(root.querySelector("[data-broken-sidebar-main]")).not.toBeNull();
    expect(root.querySelector("main")).toBeNull();
  });
});

describe("SidebarLayout", () => {
  it("pairs a named complementary landmark with one focusable main landmark", () => {
    const layout = rendered(
      render(SidebarLayout, {
        sidebarLabel: "Workspace navigation",
        sidebar: snippet('<a href="/nodes">Nodes</a>'),
        children: snippet("<h1>Node alpha</h1>"),
      }),
    );
    const aside = layout.querySelector<HTMLElement>("aside")!;
    const main = layout.querySelector<HTMLElement>("main")!;

    expect(aside.getAttribute("aria-label")).toBe("Workspace navigation");
    expect(aside.querySelector("a")?.getAttribute("href")).toBe("/nodes");
    expect(main.id).toBe("main-content");
    expect(main.tabIndex).toBe(-1);
    expect(main.textContent).toBe("Node alpha");
    expect(layout.querySelectorAll("main")).toHaveLength(1);
  });

  it("keeps landmark and focus order aligned when the sidebar changes sides", () => {
    const content = {
      sidebar: snippet('<button type="button">Navigation</button>'),
      children: snippet('<button type="button">Workspace</button>'),
    };
    const start = rendered(render(SidebarLayout, { ...content, side: "start" }));
    const end = rendered(render(SidebarLayout, { ...content, side: "end" }));

    expect([...start.children].map((child) => child.tagName)).toEqual(["ASIDE", "MAIN"]);
    expect([...end.children].map((child) => child.tagName)).toEqual(["MAIN", "ASIDE"]);
    expect([...end.querySelectorAll("button")].map((button) => button.textContent)).toEqual([
      "Workspace",
      "Navigation",
    ]);
    expect(classes(end)).toEqual(classesOf(sidebarLayout({ side: "end" }).root()));
  });

  it("composes canonical Stacks and routes structural spacing through Density", () => {
    const layout = rendered(
      render(SidebarLayout, {
        gap: "lg",
        sidebar: snippet("<p>Navigation</p>"),
        children: snippet("<p>Workspace</p>"),
      }),
    );

    // The gutter is the layout tier (ADR 0024); the regions' rhythm is the
    // caller's chosen Stack gap.
    expect(classes(layout).has("gap-[var(--reddb-spatial-layout-gap-md)]")).toBe(true);
    for (const region of layout.children) {
      const stack = region.querySelector<HTMLElement>("[data-stack]")!;
      expect(stack).not.toBeNull();
      expect(classes(stack).has("gap-[var(--reddb-spatial-gap-lg)]")).toBe(true);
      expect(stack.hasAttribute("data-density")).toBe(false);
    }
  });

  it("takes the consumer's target id, native attributes, and local class", () => {
    const layout = rendered(
      render(SidebarLayout, {
        id: "workspace-layout",
        class: "relative",
        mainId: "workspace-content",
      }),
    );

    expect(layout.id).toBe("workspace-layout");
    expect(layout.querySelector("main")?.id).toBe("workspace-content");
    expect(classes(layout)).toEqual(
      classesOf(sidebarLayout({ side: "start" }).root({ class: "relative" })),
    );
  });

  it("compiles at the public export and inherits every nested appearance axis", () => {
    const root = render(ApplicationLayoutsConsumer);
    const layout = root.querySelector<HTMLElement>("[data-sidebar-layout]")!;

    expect(layout.closest('[data-theme="base"][data-color-scheme="dark"][data-density="compact"]'))
      .not.toBeNull();
    expect(layout.hasAttribute("data-theme")).toBe(false);
    expect(layout.hasAttribute("data-color-scheme")).toBe(false);
    expect(layout.hasAttribute("data-density")).toBe(false);
    // Inside the consumer's ApplicationShell the shell owns the one <main>
    // (wave 5A); the layout's region keeps the caller's focus-target id.
    expect(root.querySelectorAll("main")).toHaveLength(1);
    expect(layout.querySelectorAll("main")).toHaveLength(0);
    expect(layout.querySelector("#deployment-content")?.tagName).toBe("DIV");
  });

  it("opts primary rails into visible labels while preserving destinations and mobile labels", () => {
    const navigation = [
      { id: "users", label: "Users", fallback: "US", href: "/users" },
      { id: "settings", label: "Application settings", fallback: "AS", href: "/settings" },
    ];
    for (const showRailLabels of [false, true]) {
      const layout = render(SidebarLayout, { navigation, currentId: "users", showRailLabels });
      const rail = layout.querySelector("[data-primary-navigation-rail]")!;
      expect(rail.querySelectorAll("[data-sidebar-rail-label]")).toHaveLength(showRailLabels ? 2 : 0);
      if (showRailLabels) expect([...rail.querySelectorAll("[data-sidebar-rail-label]")].map(label => label.textContent))
        .toEqual(navigation.map(item => item.label));
      expect(rail.querySelector('[data-sidebar-rail-item="users"]')?.getAttribute("href")).toBe("/users");
      expect(rail.querySelector('[data-sidebar-rail-item="users"]')?.getAttribute("aria-current")).toBe("page");
      const dock = layout.querySelector("[data-primary-navigation-bottom]")!;
      expect(dock.textContent).toContain("Users");
      expect(dock.textContent).toContain("Application settings");
    }
  });

  it("ships complete showcase, consumer, distributed export, and readiness evidence", () => {
    expectLayoutReadiness("sidebar-layout", "SidebarLayout");
  });
});
