import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import {
  GRID_LIST_COLUMNS,
  GridList,
  gridList,
  list,
} from "./fixtures/collection-structure-consumer";
import CollectionStructureConsumer from "./fixtures/CollectionStructureConsumer.svelte";
import { classes, classesOf, render, rendered } from "./mount";

describe("the Base GridList", () => {
  it("is available through Base", () => {
    expect(GridList).toBeDefined();
  });

  it("renders a plain div grid without list semantics when list is false (wave 5A)", () => {
    const element = rendered(render(GridList, {
      columns: 2,
      gap: "lg",
      list: false,
      children: createRawSnippet(() => ({ render: () => "<article>KPI</article>" })),
    }));

    expect(element.tagName).toBe("DIV");
    expect(element.hasAttribute("role")).toBe(false);
    expect(element.hasAttribute("data-grid-list")).toBe(true);
    expect(classes(element)).toEqual(classesOf(list({ gap: "lg", class: gridList({ columns: 2 }) })));
  });

  it("composes List semantics with a caller-selected responsive grid", () => {
    expect(GRID_LIST_COLUMNS).toEqual([1, 2, 3, 4]);
    const element = rendered(render(GridList, {
      columns: 3,
      gap: "sm",
      class: "min-w-0",
      "aria-label": "Services",
      children: createRawSnippet(() => ({
        render: () => '<li><a href="/service">API</a><button>Restart</button></li>',
      })),
    }));

    expect(element.tagName).toBe("UL");
    expect(element.hasAttribute("data-list")).toBe(true);
    expect(element.getAttribute("aria-label")).toBe("Services");
    expect(classes(element)).toEqual(classesOf(list({
      gap: "sm",
      class: gridList({ columns: 3, class: "min-w-0" }),
    })));
    expect([...element.querySelectorAll<HTMLElement>("a, button")].map((item) => item.textContent))
      .toEqual(["API", "Restart"]);
    expect(element.tabIndex).toBe(-1);
    expect(element.hasAttribute("data-density")).toBe(false);
  });

  it("inherits every nested appearance axis", () => {
    const element = render(CollectionStructureConsumer)
      .querySelector<HTMLElement>("[data-nested-grid-list]")!;

    expect(element.hasAttribute("data-theme")).toBe(false);
    expect(element.hasAttribute("data-color-scheme")).toBe(false);
    expect(element.hasAttribute("data-density")).toBe(false);
  });
});
