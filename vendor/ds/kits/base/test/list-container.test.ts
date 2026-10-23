import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import { ListContainer, card } from "./fixtures/collection-structure-consumer";
import CollectionStructureConsumer from "./fixtures/CollectionStructureConsumer.svelte";
import { classes, classesOf, render, rendered } from "./mount";

describe("the Base ListContainer", () => {
  it("is available through Base", () => {
    expect(ListContainer).toBeDefined();
  });

  it("composes the canonical Card while preserving caller-owned list controls", () => {
    const element = rendered(render(ListContainer, {
      title: "Deployments",
      description: "Recent environments",
      variant: "raised",
      padding: "sm",
      class: "min-w-0",
      "aria-label": "Deployment list",
      children: createRawSnippet(() => ({
        render: () => '<ul><li><a href="/alpha">Alpha</a></li><li><button>Retry</button></li></ul>',
      })),
    }));

    expect(element.hasAttribute("data-card")).toBe(true);
    expect(element.textContent).toContain("Deployments");
    expect(element.getAttribute("aria-label")).toBe("Deployment list");
    expect(classes(element)).toEqual(classesOf(card({
      variant: "raised",
      padding: "sm",
    }).root({ class: "min-w-0" })));
    expect([...element.querySelectorAll<HTMLElement>("a, button")].map((item) => item.textContent))
      .toEqual(["Alpha", "Retry"]);
    expect(element.tabIndex).toBe(-1);
  });

  it("inherits every nested appearance axis", () => {
    const element = render(CollectionStructureConsumer)
      .querySelector<HTMLElement>("[data-nested-list-container]")!;

    expect(element.hasAttribute("data-theme")).toBe(false);
    expect(element.hasAttribute("data-color-scheme")).toBe(false);
    expect(element.hasAttribute("data-density")).toBe(false);
  });
});
