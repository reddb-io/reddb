import { describe, expect, it } from "vitest";
import { Breadcrumbs, breadcrumbs as breadcrumbsAppearance } from "@reddb-io/design-system/base";
import WayfindingContractFailures from "./fixtures/WayfindingContractFailures.svelte";
import WayfindingConsumer from "./fixtures/WayfindingConsumer.svelte";
import { classes, render } from "./mount";

describe("the deliberately failing Breadcrumbs fixture", () => {
  it("demonstrates breadcrumbs without an announced current page", () => {
    const root = render(WayfindingContractFailures, { failure: "breadcrumbs-current" });

    expect(root.querySelector("[aria-current]")).toBeNull();
  });

  it("keeps token appearance live inside a nested appearance scope", () => {
    const scope = render(WayfindingConsumer).querySelector<HTMLElement>("[data-wayfinding-scope]")!;
    const root = scope.querySelector<HTMLElement>("[data-breadcrumbs]")!;

    expect(classes(root).has("text-caption")).toBe(true);
    expect(classes(root)).toEqual(new Set(breadcrumbsAppearance().root().split(/\s+/)));
    expect(classes(root.querySelector("ol")!).has("gap-[var(--reddb-spatial-gap-lg)]")).toBe(true);
    expect(scope.getAttribute("data-density")).toBe("compact");
    expect(root.hasAttribute("data-theme")).toBe(false);
    expect(root.hasAttribute("data-color-scheme")).toBe(false);
    expect(root.hasAttribute("data-contrast")).toBe(false);
    expect(root.hasAttribute("data-density")).toBe(false);
  });
});

describe("the Base Breadcrumbs", () => {
  it("announces the current page while earlier destinations remain native links", () => {
    const root = render(Breadcrumbs, {
      items: [
        { id: "clusters", label: "Clusters", href: "/clusters" },
        { id: "primary", label: "Primary cluster", current: true },
      ],
    });
    const nav = root.querySelector<HTMLElement>("nav")!;
    const current = nav.querySelector<HTMLElement>('[aria-current="page"]')!;

    expect(nav.getAttribute("aria-label")).toBe("Breadcrumbs");
    expect(nav.querySelector('a[href="/clusters"]')?.textContent).toBe("Clusters");
    expect(current.tagName).toBe("SPAN");
    expect(current.textContent).toBe("Primary cluster");
  });
});
