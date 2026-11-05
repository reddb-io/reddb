import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import ProductFeature from "../src/composites/ProductFeature.svelte";
import { productFeature } from "../src/composites/product-feature.variants";
import ProductCatalogueConsumer from "./fixtures/ProductCatalogueConsumer.svelte";
import { expectLayoutReadiness } from "./layout-readiness";
import { classes, classesOf, render, rendered } from "./mount";

const media = createRawSnippet(() => ({
  render: () => '<img src="/binding.jpg" alt="Thread binding detail">',
}));

describe("ProductFeature", () => {
  it("composes canonical MediaObject and SectionHeading contracts", () => {
    const children = createRawSnippet(() => ({
      render: () => "<p>Each notebook opens flat on a desk.</p>",
    }));
    const feature = rendered(
      render(ProductFeature, {
        title: "Thread-bound",
        description: "Built for daily use.",
        media,
        children,
        mediaSide: "end",
      }),
    );

    expect(feature.matches("[data-media-object][data-product-feature][role=region]")).toBe(true);
    expect(feature.querySelector("[data-section-heading] h2")?.textContent).toBe("Thread-bound");
    expect(feature.getAttribute("aria-labelledby")).toBe(
      feature.querySelector("[data-section-heading] h2")?.id,
    );
    expect(feature.querySelector("[data-media-object-media] img")?.getAttribute("alt")).toBe(
      "Thread binding detail",
    );
    expect(feature.querySelector("[data-product-feature-body]")?.textContent).toContain(
      "opens flat",
    );
    // The media trails the content, stacked or side by side, and the two share
    // a line from the feature's own `3xl` container step (ADR 0021).
    const mediaRegion = feature.querySelector<HTMLElement>("[data-media-object-media]")!;
    expect(classes(mediaRegion).has("order-last")).toBe(true);
    expect(classes(feature).has("@container/product-feature")).toBe(true);
    expect(classes(mediaRegion).has("@3xl/product-feature:basis-0")).toBe(true);
  });

  it("preserves focus for caller-owned feature actions", () => {
    const actions = createRawSnippet(() => ({
      render: () => '<a href="/materials">Explore materials</a>',
    }));
    const feature = rendered(
      render(ProductFeature, { title: "Thread-bound", media, actions }),
    );
    const link = feature.querySelector<HTMLAnchorElement>("a")!;

    link.focus();
    expect(document.activeElement).toBe(link);
  });

  it("routes feature spacing through Density and inherits nested appearance", () => {
    const root = render(ProductCatalogueConsumer);
    const feature = root.querySelector<HTMLElement>("[data-product-feature]")!;

    expect(classes(feature)).toEqual(classesOf(productFeature({ mediaSide: "end" }).root()));
    expect(classes(feature).has("gap-[var(--reddb-spatial-layout-gap-md)]")).toBe(true);
    expect(feature.closest('[data-theme="base"][data-color-scheme="dark"][data-density="compact"]'))
      .not.toBeNull();
    expect(feature.hasAttribute("data-theme")).toBe(false);
    expect(feature.hasAttribute("data-color-scheme")).toBe(false);
    expect(feature.hasAttribute("data-density")).toBe(false);
  });

  it("ships showcase, consumer, distributed export, and readiness evidence", () => {
    expectLayoutReadiness("product-feature", "ProductFeature");
  });
});
