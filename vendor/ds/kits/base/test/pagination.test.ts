import { describe, expect, it } from "vitest";
import { Pagination, pagination as paginationAppearance } from "@reddb-io/design-system/base";
import WayfindingContractFailures from "./fixtures/WayfindingContractFailures.svelte";
import WayfindingConsumer from "./fixtures/WayfindingConsumer.svelte";
import { classes, render } from "./mount";

describe("the deliberately failing Pagination fixture", () => {
  it("demonstrates pagination without accessible page labels", () => {
    const root = render(WayfindingContractFailures, { failure: "pagination-labels" });
    const pages = [...root.querySelectorAll<HTMLAnchorElement>("a")];

    expect(pages.map((page) => page.getAttribute("aria-label"))).toEqual([null, null]);
  });
});

describe("the Base Pagination", () => {
  it("names every page and announces the current page without colour", () => {
    const root = render(Pagination, {
      pages: [
        { page: 1, href: "?page=1" },
        { page: 2, href: "?page=2", current: true },
        { page: 3, href: "?page=3" },
      ],
    });
    const links = [...root.querySelectorAll<HTMLAnchorElement>("a")];

    expect(links.map((link) => link.getAttribute("aria-label"))).toEqual([
      "Go to page 1",
      "Page 2, current page",
      "Go to page 3",
    ]);
    expect(links[1]!.getAttribute("aria-current")).toBe("page");
    links[2]!.focus();
    expect(document.activeElement).toBe(links[2]);
  });

  it("keeps token appearance live inside a nested appearance scope", () => {
    const scope = render(WayfindingConsumer).querySelector<HTMLElement>("[data-wayfinding-scope]")!;
    const root = scope.querySelector<HTMLElement>("[data-pagination]")!;
    const current = root.querySelector<HTMLElement>('[aria-current="page"]')!;

    expect(classes(root)).toEqual(new Set(paginationAppearance().root().split(/\s+/)));
    expect(classes(current).has("h-[var(--reddb-spatial-control-height-sm)]")).toBe(true);
    expect(scope.getAttribute("data-density")).toBe("compact");
    expect(root.hasAttribute("data-theme")).toBe(false);
    expect(root.hasAttribute("data-color-scheme")).toBe(false);
    expect(root.hasAttribute("data-contrast")).toBe(false);
    expect(root.hasAttribute("data-density")).toBe(false);
  });
});
