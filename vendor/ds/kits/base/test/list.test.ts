import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import CollectionStructureContractFailures from "./fixtures/CollectionStructureContractFailures.svelte";
import CollectionStructureConsumer from "./fixtures/CollectionStructureConsumer.svelte";
import { LIST_GAPS, List, list } from "./fixtures/collection-structure-consumer";
import { classes, classesOf, render, rendered } from "./mount";

describe("the deliberately failing List fixture", () => {
  it("diagnoses a visual collection without list semantics", () => {
    const element = rendered(render(CollectionStructureContractFailures, {
      failure: "missing-list-semantics",
    }));

    expect(element.matches("ul, ol")).toBe(false);
    expect(element.getAttribute("role")).not.toBe("list");
  });
});

describe("the Base List", () => {
  it("is available through Base", () => {
    expect(List).toBeDefined();
  });

  it("renders unordered and ordered collections with native list semantics", () => {
    const children = createRawSnippet(() => ({
      render: () => "<li>Alpha</li>",
    }));
    const unordered = rendered(render(List, { children }));
    const ordered = rendered(render(List, { children, ordered: true }));

    expect(unordered.tagName).toBe("UL");
    expect(ordered.tagName).toBe("OL");
    expect(ordered.firstElementChild?.textContent).toBe("Alpha");
  });

  it("keeps Density-owned spacing in its public appearance seam", () => {
    expect(LIST_GAPS).toEqual(["sm", "md", "lg"]);
    const element = rendered(render(List, {
      gap: "lg",
      class: "min-w-0",
      "aria-label": "Results",
    }));

    expect(list).toBeTypeOf("function");
    expect(classes(element)).toEqual(classesOf(list({ gap: "lg", class: "min-w-0" })));
    expect(element.getAttribute("aria-label")).toBe("Results");
    expect(element.hasAttribute("data-density")).toBe(false);
  });

  it("inherits nested appearance without changing caller-owned focus order", () => {
    const element = render(CollectionStructureConsumer)
      .querySelector<HTMLElement>("[data-nested-list]")!;

    expect(element.hasAttribute("data-theme")).toBe(false);
    expect(element.hasAttribute("data-color-scheme")).toBe(false);
    expect(element.hasAttribute("data-density")).toBe(false);
    expect([...element.querySelectorAll<HTMLElement>("a, button")].map((item) => item.textContent))
      .toEqual(["Alpha", "Retry"]);
  });
});
