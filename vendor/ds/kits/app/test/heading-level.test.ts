// Wave 5A: composites that name their items with a heading take the outline
// level from the caller, so a page outline never skips a level (axe
// heading-order). The default stays the level each composite always used.

import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import CategoryPreview from "../src/composites/CategoryPreview.svelte";
import ProductList from "../src/composites/ProductList.svelte";
import { render, rendered } from "./mount";

const media = createRawSnippet(() => ({ render: () => '<img src="/desks.jpg" alt="Oak writing desk">' }));
const items = [
  { id: "notebook", name: "Dot grid notebook", price: "$18" },
  { id: "pen", name: "Fountain pen", price: "$42" },
];

describe("headingLevel", () => {
  it("defaults CategoryPreview's name to h3 and follows the caller from 2 to 6", () => {
    const preview = (headingLevel?: 2 | 3 | 4 | 5 | 6) =>
      rendered(render(CategoryPreview, { name: "Desks", href: "/desks", media, headingLevel }));

    expect(preview().querySelector("[data-heading]")?.tagName).toBe("H3");
    for (const level of [2, 3, 4, 5, 6] as const) {
      expect(preview(level).querySelector("[data-heading]")?.tagName).toBe(`H${level}`);
    }
  });

  it("defaults every ProductList name to h3 and follows the caller from 2 to 6", () => {
    const list = (headingLevel?: 2 | 3 | 4 | 5 | 6) =>
      rendered(render(ProductList, { label: "Stationery", items, headingLevel }));

    expect([...list().querySelectorAll("[data-product-list-name]")].map((name) => name.tagName)).toEqual(["H3", "H3"]);
    for (const level of [2, 3, 4, 5, 6] as const) {
      const names = [...list(level).querySelectorAll("[data-product-list-name]")].map((name) => name.tagName);
      expect(names).toEqual([`H${level}`, `H${level}`]);
    }
  });
});
