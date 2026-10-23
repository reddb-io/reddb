import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import BottomNavigation from "../src/composites/BottomNavigation.svelte";
import SidebarNavigation from "../src/composites/SidebarNavigation.svelte";
import StoreNavigation from "../src/composites/StoreNavigation.svelte";
import { bottomNavigation } from "../src/composites/bottom-navigation.variants";
import { navigationItems } from "../src/composites/navigation.behavior";
import { sidebarNavigation } from "../src/composites/sidebar-navigation.variants";
import { storeNavigation } from "../src/composites/store-navigation.variants";
import NavigationSurfacesConsumer from "./fixtures/NavigationSurfacesConsumer.svelte";
import NavigationContractFailures from "./fixtures/NavigationContractFailures.svelte";
import TestGlyph from "./fixtures/TestGlyph.svelte";
import { render, rendered } from "./mount";

const ITEMS = [
  { id: "overview", label: "Overview", href: "/overview" },
  { id: "activity", label: "Activity", href: "/activity" },
  { id: "settings", label: "Settings", href: "/settings" },
] as const;
const REPO_ROOT = join(import.meta.dirname, "..", "..", "..");

describe("the shared current-item contract", () => {
  it("rejects navigation unless currentId identifies exactly one destination", () => {
    expect(() => navigationItems(ITEMS, "missing")).toThrow(
      'currentId "missing" must identify exactly one navigation item',
    );
    expect(() =>
      navigationItems([...ITEMS, { id: "overview", label: "Duplicate", href: "/duplicate" }], "overview"),
    ).toThrow('currentId "overview" must identify exactly one navigation item');
  });
});

describe("the deliberately failing navigation fixtures", () => {
  it("exposes sidebar navigation with no announced current item", () => {
    const broken = rendered(
      render(NavigationContractFailures, { failure: "sidebar-without-current" }),
    );

    expect(broken.querySelectorAll('[aria-current="page"]')).toHaveLength(0);
  });

  it("exposes a bottom bar with no keyboard-reachable destination", () => {
    const broken = rendered(
      render(NavigationContractFailures, { failure: "bottom-unreachable-by-keyboard" }),
    );

    expect(broken.querySelectorAll('a[href], button, [tabindex="0"]')).toHaveLength(0);
  });
});

describe("BottomNavigation", () => {
  it("keeps every destination in native focus order and announces the current one", () => {
    const navigation = rendered(
      render(BottomNavigation, {
        label: "Primary",
        items: ITEMS,
        currentId: "overview",
      }),
    );
    const links = [...navigation.querySelectorAll<HTMLAnchorElement>("[data-navigation-item]")];

    expect(navigation.tagName).toBe("NAV");
    expect(links.map((link) => link.tabIndex)).toEqual([0, 0, 0]);
    for (const link of links) {
      link.focus();
      expect(document.activeElement).toBe(link);
    }
    expect(links.map((link) => link.getAttribute("aria-current"))).toEqual([
      "page",
      null,
      null,
    ]);
  });
});

describe("navigation destination glyphs", () => {
  it("draws an optional glyph through the Base Icon contract on sidebar and bottom surfaces", () => {
    const items = [
      { id: "overview", label: "Overview", href: "/overview", icon: TestGlyph },
      { id: "activity", label: "Activity", href: "/activity" },
    ];

    for (const surface of [SidebarNavigation, BottomNavigation]) {
      const navigation = rendered(
        render(surface, { label: "Primary", items, currentId: "overview" }),
      );
      const [withGlyph, withoutGlyph] = [
        ...navigation.querySelectorAll<HTMLElement>("[data-navigation-item]"),
      ];

      expect(withGlyph?.querySelector("svg[data-icon]")?.getAttribute("aria-hidden")).toBe("true");
      expect(withGlyph?.querySelector("svg[data-icon]")?.getAttribute("color")).toBe(
        "var(--reddb-color-foreground)",
      );
      expect(withGlyph?.textContent?.trim()).toBe("Overview");
      expect(withoutGlyph?.querySelector("svg")).toBeNull();
      expect(withoutGlyph?.querySelector('[aria-hidden="true"]')).toBeNull();
    }
  });
});

describe("StoreNavigation", () => {
  it("composes the canonical Navbar around caller-owned store regions and one current item", () => {
    const navigation = rendered(
      render(StoreNavigation, {
        label: "Store",
        items: ITEMS,
        currentId: "settings",
        collapse: "expanded",
        announcement: createRawSnippet(() => ({ render: () => "<span>Free delivery</span>" })),
        brand: createRawSnippet(() => ({ render: () => '<a href="/">Acme</a>' })),
        actions: createRawSnippet(() => ({ render: () => '<a href="/cart">Cart</a>' })),
      }),
    );
    const navbar = navigation.querySelector<HTMLElement>("[data-navbar]")!;
    const links = [...navbar.querySelectorAll<HTMLAnchorElement>("[data-navbar-link]")];

    expect(navigation.dataset.storeNavigation).toBe("");
    expect(navigation.firstElementChild?.dataset.storeAnnouncement).toBe("");
    expect(navigation.firstElementChild?.textContent).toBe("Free delivery");
    expect(navbar.getAttribute("aria-label")).toBe("Store");
    expect(navbar.querySelector('[data-navbar-region="brand"]')?.textContent).toBe("Acme");
    expect(navbar.querySelector('[data-navbar-region="actions"]')?.textContent).toBe("Cart");
    expect(links.map((link) => link.getAttribute("aria-current"))).toEqual([
      null,
      null,
      "page",
    ]);
  });
});

describe("the three navigation surfaces as a public consumer", () => {
  it("compile together and inherit every nested appearance axis", () => {
    const scope = rendered(render(NavigationSurfacesConsumer));
    const surfaces = scope.querySelectorAll<HTMLElement>(
      "[data-sidebar-navigation], [data-bottom-navigation], [data-store-navigation]",
    );

    expect(scope.matches('[data-theme="application"][data-color-scheme="dark"][data-density="compact"]'))
      .toBe(true);
    expect(surfaces).toHaveLength(3);
    for (const surface of surfaces) {
      expect(surface.hasAttribute("data-theme")).toBe(false);
      expect(surface.hasAttribute("data-color-scheme")).toBe(false);
      expect(surface.hasAttribute("data-density")).toBe(false);
    }
    expect([...scope.querySelectorAll<HTMLAnchorElement>("a[href]")].every((link) => link.tabIndex === 0))
      .toBe(true);
  });

  it("route every spatial value through Density-owned roles", () => {
    for (const classes of [
      sidebarNavigation().list(),
      sidebarNavigation().item(),
      bottomNavigation().list(),
      bottomNavigation().item(),
      storeNavigation().announcement(),
    ]) {
      expect(classes).toContain("var(--reddb-spatial-");
      expect(classes).not.toMatch(/(?:^|\s)(?:h|p[trblxy]?|gap)-[1-9]/);
    }
  });
});

describe("navigation release readiness", () => {
  const capabilities = [
    ["sidebar-navigation", "SidebarNavigation"],
    ["bottom-navigation", "BottomNavigation"],
    ["store-navigation", "StoreNavigation"],
  ] as const;

  for (const [id, exported] of capabilities) {
    it(`${id} is implemented, showcased, consumer-compiled, and distributed`, () => {
      const definitions = JSON.parse(
        readFileSync(join(REPO_ROOT, "scripts", "producer", "readiness.json"), "utf8"),
      ) as Array<{
        id: string;
        baselineDisposition: string;
        canonicalKit: string;
        export: string;
        evidence: Record<string, string[]>;
      }>;
      const definition = definitions.find((candidate) => candidate.id === id);

      expect(definition).toMatchObject({
        id,
        baselineDisposition: "implemented",
        canonicalKit: "application",
        export: exported,
      });
      expect(Object.keys(definition!.evidence).sort()).toEqual([
        "accessibility",
        "appearance",
        "behavior",
        "consumerCompilation",
        "documentation",
        "showcase",
      ]);
      for (const paths of Object.values(definition!.evidence)) {
        expect(paths.length).toBeGreaterThan(0);
        for (const path of paths) expect(existsSync(join(REPO_ROOT, path)), path).toBe(true);
      }

      const snapshot = JSON.parse(
        readFileSync(
          join(REPO_ROOT, "packages", "baseline", "snapshot", "baseline-v1.json"),
          "utf8",
        ),
      ) as { capabilities: Array<{ id: string; classification: string; status: string }> };
      expect(snapshot.capabilities.find((capability) => capability.id === id)).toMatchObject({
        classification: "application",
        status: "implemented",
      });

      const barrel = readFileSync(join(REPO_ROOT, "kits", "app", "src", "index.ts"), "utf8");
      expect(barrel).toContain(`export { default as ${exported} }`);
    });
  }
});

describe("SidebarNavigation", () => {
  it("renders a named native navigation list with exactly one current item", () => {
    const navigation = rendered(
      render(SidebarNavigation, {
        label: "Workspace",
        items: ITEMS,
        currentId: "activity",
      }),
    );
    const links = [...navigation.querySelectorAll<HTMLAnchorElement>("[data-navigation-item]")];

    expect(navigation.tagName).toBe("NAV");
    expect(navigation.getAttribute("aria-label")).toBe("Workspace");
    expect(navigation.querySelector("ul")?.children).toHaveLength(3);
    expect(links.map((link) => link.getAttribute("href"))).toEqual([
      "/overview",
      "/activity",
      "/settings",
    ]);
    expect(links.filter((link) => link.getAttribute("aria-current") === "page"))
      .toEqual([links[1]]);
  });
});
