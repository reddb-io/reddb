import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import CategoryPreview from "../src/composites/CategoryPreview.svelte";
import { categoryPreview } from "../src/composites/category-preview.variants";
import MerchandisingContractFailures from "./fixtures/MerchandisingContractFailures.svelte";
import MerchandisingSurfacesConsumer from "./fixtures/MerchandisingSurfacesConsumer.svelte";
import { expectLayoutReadiness } from "./layout-readiness";
import { classes, classesOf, render, rendered } from "./mount";

describe("the deliberately failing category preview fixture", () => {
  it("does not expose a discernible link target", () => {
    const root = render(MerchandisingContractFailures);
    const link = root.querySelector<HTMLAnchorElement>("[data-indiscernible-category-preview-link]")!;

    expect(link.textContent?.trim()).toBe("");
    expect(link.getAttribute("aria-label")).toBeNull();
    expect(link.getAttribute("aria-labelledby")).toBeNull();
    expect(link.querySelector("[alt]")).toBeNull();
  });
});

describe("CategoryPreview", () => {
  it("composes canonical Card, AspectRatio, and Link contracts into a discernible destination", () => {
    const media = createRawSnippet(() => ({
      render: () => '<img src="/desks.jpg" alt="Oak writing desk">',
    }));
    const preview = rendered(
      render(CategoryPreview, {
        name: "Desks",
        href: "/categories/desks",
        description: "Workspaces for every room.",
        media,
      }),
    );
    const link = preview.querySelector<HTMLAnchorElement>("[data-link]")!;

    expect(preview.matches("[data-card][data-category-preview][role=article]")).toBe(true);
    expect(preview.querySelector("[data-aspect-ratio] img")?.getAttribute("alt")).toBe(
      "Oak writing desk",
    );
    expect(link.getAttribute("href")).toBe("/categories/desks");
    expect(link.textContent).toBe("Desks");
    expect(preview.getAttribute("aria-labelledby")).toBe(link.id);
    expect(preview.textContent).toContain("Workspaces for every room.");
  });

  it("keeps the native category destination keyboard reachable", () => {
    const root = render(MerchandisingSurfacesConsumer);
    const link = root.querySelector<HTMLAnchorElement>("[data-category-preview] a")!;

    link.focus();
    expect(document.activeElement).toBe(link);
  });

  it("routes preview rhythm through Density and inherits every nested appearance axis", () => {
    const root = render(MerchandisingSurfacesConsumer);
    const preview = root.querySelector<HTMLElement>("[data-category-preview]")!;
    const content = preview.querySelector<HTMLElement>("[data-category-preview-content]")!;

    expect(classes(preview)).toEqual(classesOf(categoryPreview().root()));
    expect(classes(content)).toContain("gap-[var(--reddb-spatial-gap-sm)]");
    expect(classes(content)).toContain("pt-[var(--reddb-spatial-inset-sm)]");
    expect(preview.closest('[data-theme="base"][data-color-scheme="dark"][data-density="compact"]'))
      .not.toBeNull();
    expect(preview.hasAttribute("data-theme")).toBe(false);
    expect(preview.hasAttribute("data-color-scheme")).toBe(false);
    expect(preview.hasAttribute("data-density")).toBe(false);
  });

  it("ships showcase, consumer, distributed export, and readiness evidence", () => {
    expect(render(MerchandisingSurfacesConsumer).querySelector("[data-category-preview]"))
      .not.toBeNull();
    expectLayoutReadiness("category-preview", "CategoryPreview");
  });
});
