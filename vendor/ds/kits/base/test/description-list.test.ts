import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import CollectionStructureContractFailures from "./fixtures/CollectionStructureContractFailures.svelte";
import CollectionStructureConsumer from "./fixtures/CollectionStructureConsumer.svelte";
import {
  DescriptionList,
  DESCRIPTION_LIST_GAPS,
  descriptionList,
  type DescriptionListItem,
} from "./fixtures/collection-structure-consumer";
import { classes, classesOf, render, rendered } from "./mount";

function hasTermDetailPairs(element: HTMLElement): boolean {
  const entries = [...element.children];
  return entries.length > 0 && entries.every((entry) => {
    const children = [...entry.children];
    return children.length === 2 && children[0]?.tagName === "DT" && children[1]?.tagName === "DD";
  });
}

describe("the deliberately failing DescriptionList fixture", () => {
  it("diagnoses broken term/detail pairing", () => {
    const element = rendered(render(CollectionStructureContractFailures, {
      failure: "broken-term-detail-pairing",
    }));

    expect(element.tagName).toBe("DL");
    expect(hasTermDetailPairs(element)).toBe(false);
  });
});

describe("the Base DescriptionList", () => {
  it("is available through Base", () => {
    expect(DescriptionList).toBeDefined();
  });

  it("owns one native term/detail pair per caller entry", () => {
    const items: readonly DescriptionListItem[] = [
      { term: "Region", detail: "South America" },
      {
        term: "Owner",
        detail: createRawSnippet(() => ({ render: () => '<a href="/team">Platform team</a>' })),
      },
    ];
    const element = rendered(render(DescriptionList, {
      items,
      "aria-label": "Service details",
    }));

    expect(element.tagName).toBe("DL");
    expect(hasTermDetailPairs(element)).toBe(true);
    expect([...element.querySelectorAll("dt")].map((term) => term.textContent?.trim()))
      .toEqual(["Region", "Owner"]);
    expect(element.querySelector("a")?.textContent).toBe("Platform team");
    expect(element.getAttribute("aria-label")).toBe("Service details");
    expect(element.tabIndex).toBe(-1);
  });

  it("keeps Density-owned pairing space in its public appearance seam", () => {
    expect(DESCRIPTION_LIST_GAPS).toEqual(["sm", "md", "lg"]);
    const element = rendered(render(DescriptionList, {
      items: [{ term: "Region", detail: "South America" }],
      gap: "lg",
      class: "min-w-0",
    }));
    const slots = descriptionList({ gap: "lg" });

    expect(classes(element)).toEqual(classesOf(slots.root({ class: "min-w-0" })));
    expect(classes(element.querySelector("[data-description-item]")!))
      .toEqual(classesOf(slots.item()));
    expect(element.hasAttribute("data-theme")).toBe(false);
    expect(element.hasAttribute("data-color-scheme")).toBe(false);
    expect(element.hasAttribute("data-density")).toBe(false);
  });

  it("inherits every nested appearance axis", () => {
    const element = render(CollectionStructureConsumer)
      .querySelector<HTMLElement>("[data-nested-description-list]")!;

    expect(element.hasAttribute("data-theme")).toBe(false);
    expect(element.hasAttribute("data-color-scheme")).toBe(false);
    expect(element.hasAttribute("data-density")).toBe(false);
  });
});
