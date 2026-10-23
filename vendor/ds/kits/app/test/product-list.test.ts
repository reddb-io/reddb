import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import ProductList from "../src/composites/ProductList.svelte";
import { productList } from "../src/composites/product-list.variants";
import ProductCatalogueConsumer from "./fixtures/ProductCatalogueConsumer.svelte";
import ProductCatalogueContractFailures from "./fixtures/ProductCatalogueContractFailures.svelte";
import { expectLayoutReadiness } from "./layout-readiness";
import { classes, classesOf, render, rendered } from "./mount";

describe("the deliberately failing product list fixture", () => {
  it("has neither a collection name nor a name for its icon-only action", () => {
    const root = render(ProductCatalogueContractFailures);
    const list = root.querySelector("[data-unnamed-product-list]")!;
    const action = root.querySelector("[data-unnamed-product-action]")!;

    expect(list.getAttribute("aria-label")).toBeNull();
    expect(list.getAttribute("aria-labelledby")).toBeNull();
    expect(action.textContent?.trim()).toBe("");
    expect(action.getAttribute("aria-label")).toBeNull();
  });
});

describe("ProductList", () => {
  it("composes canonical GridList and Card contracts into a named product collection", () => {
    const actions = createRawSnippet(() => ({
      render: () => '<button type="button">Add Field notes to basket</button>',
    }));
    const list = rendered(
      render(ProductList, {
        label: "Stationery",
        columns: 3,
        items: [
          {
            id: "field-notes",
            name: "Field notes",
            href: "/products/field-notes",
            description: "A pocket notebook.",
            price: "$12",
            actions,
          },
          { id: "desk-pad", name: "Desk pad", price: "$18" },
        ],
      }),
    );

    expect(list.matches("ul[data-grid-list][data-product-list]")).toBe(true);
    expect(list.getAttribute("aria-label")).toBe("Stationery");
    expect(list.querySelectorAll("[data-card][data-product-list-item]")).toHaveLength(2);
    expect(list.querySelector("a")?.getAttribute("href")).toBe("/products/field-notes");
    expect(list.querySelector("a")?.textContent).toBe("Field notes");
    const item = list.querySelector("[data-product-list-item]")!;
    expect(item.getAttribute("aria-labelledby")).toBe(
      item.querySelector("[data-product-list-name]")?.id,
    );
  });

  it("preserves native product-link and action focus order", () => {
    const actions = createRawSnippet(() => ({
      render: () => '<button type="button">Add to basket</button>',
    }));
    const list = rendered(
      render(ProductList, {
        label: "Stationery",
        items: [{ id: "field-notes", name: "Field notes", href: "/notes", actions }],
      }),
    );
    const controls = [...list.querySelectorAll<HTMLElement>("a, button")];

    expect(controls.map((control) => control.textContent)).toEqual([
      "Field notes",
      "Add to basket",
    ]);
    for (const control of controls) {
      control.focus();
      expect(document.activeElement).toBe(control);
    }
  });

  it("routes collection rhythm through Density and inherits nested appearance", () => {
    const root = render(ProductCatalogueConsumer);
    const list = root.querySelector<HTMLElement>("[data-product-list]")!;

    expect(classes(list)).toEqual(classesOf(productList().root()));
    expect(classes(list).has("gap-[var(--reddb-spatial-gap-md)]")).toBe(true);
    expect(list.closest('[data-theme="base"][data-color-scheme="dark"][data-density="compact"]'))
      .not.toBeNull();
    expect(list.hasAttribute("data-theme")).toBe(false);
    expect(list.hasAttribute("data-color-scheme")).toBe(false);
    expect(list.hasAttribute("data-density")).toBe(false);
  });

  it("ships showcase, consumer, distributed export, and readiness evidence", () => {
    expectLayoutReadiness("product-list", "ProductList");
  });
});
