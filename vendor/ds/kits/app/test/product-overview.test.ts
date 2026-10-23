import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import ProductOverview from "../src/composites/ProductOverview.svelte";
import { productOverview } from "../src/composites/product-overview.variants";
import ProductCatalogueConsumer from "./fixtures/ProductCatalogueConsumer.svelte";
import { expectLayoutReadiness } from "./layout-readiness";
import { classes, classesOf, render, rendered } from "./mount";

const media = createRawSnippet(() => ({
  render: () => '<img src="/field-notes.jpg" alt="Black field-notes book">',
}));

describe("ProductOverview", () => {
  it("composes canonical heading and stack contracts without owning product copy", () => {
    const children = createRawSnippet(() => ({ render: () => "<p>Dot-grid paper.</p>" }));
    const actions = createRawSnippet(() => ({
      render: () => '<button type="button">Add to basket</button>',
    }));
    const overview = rendered(
      render(ProductOverview, {
        title: "Field notes",
        description: "A pocket notebook.",
        price: "$12",
        media,
        children,
        actions,
      }),
    );

    expect(overview.matches("section[data-product-overview][aria-labelledby]")).toBe(true);
    expect(overview.querySelector("[data-section-heading] h2")?.textContent).toBe("Field notes");
    expect(overview.getAttribute("aria-labelledby")).toBe(
      overview.querySelector("[data-section-heading] h2")?.id,
    );
    expect(overview.querySelector("[data-product-overview-media] img")?.getAttribute("alt")).toBe(
      "Black field-notes book",
    );
    expect(overview.querySelector("[data-product-overview-price]")?.textContent).toBe("$12");
    expect(overview.querySelector("[data-product-overview-body]")?.textContent).toContain(
      "Dot-grid paper.",
    );
  });

  it("keeps caller-owned actions in native focus order", () => {
    const actions = createRawSnippet(() => ({
      render: () =>
        '<span><a href="/shipping">Shipping</a><button type="button">Add to basket</button></span>',
    }));
    const overview = rendered(render(ProductOverview, { title: "Field notes", media, actions }));
    const controls = [...overview.querySelectorAll<HTMLElement>("a, button")];

    expect(controls.map((control) => control.textContent)).toEqual(["Shipping", "Add to basket"]);
    for (const control of controls) {
      control.focus();
      expect(document.activeElement).toBe(control);
    }
  });

  it("routes layout rhythm through Density and inherits nested appearance", () => {
    const root = render(ProductCatalogueConsumer);
    const overview = root.querySelector<HTMLElement>("[data-product-overview]")!;

    expect(classes(overview)).toEqual(classesOf(productOverview().root()));
    expect(classes(overview).has("gap-[var(--reddb-spatial-layout-gap-md)]")).toBe(true);
    // Its two columns step on its own width, never the window's (ADR 0021).
    expect([...classes(overview)].some((name) => /^(?:max-)?(?:sm|md|lg|xl):/.test(name))).toBe(false);
    expect(overview.closest('[data-theme="base"][data-color-scheme="dark"][data-density="compact"]'))
      .not.toBeNull();
    expect(overview.hasAttribute("data-theme")).toBe(false);
    expect(overview.hasAttribute("data-color-scheme")).toBe(false);
    expect(overview.hasAttribute("data-density")).toBe(false);
  });

  it("ships showcase, consumer, distributed export, and readiness evidence", () => {
    expectLayoutReadiness("product-overview", "ProductOverview");
  });
});

describe("ProductOverview image", () => {
  it("renders the product image through the Base Image, with its priority", () => {
    const overview = rendered(
      render(ProductOverview, {
        title: "Field notes",
        image: { src: "/notes.webp", alt: "Field-notes book", width: 800, height: 800 },
        priority: true,
      }),
    );
    const element = overview.querySelector("[data-product-overview-media] img")!;
    expect(element.getAttribute("width")).toBe("800");
    expect(element.getAttribute("fetchpriority")).toBe("high");
  });
});
