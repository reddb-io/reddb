import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import SurfaceControlsConsumer from "./fixtures/SurfaceControlsConsumer.svelte";
import {
  SECTION_HEADING_LEVELS,
  SECTION_HEADING_SIZES,
  SectionHeading,
  sectionHeading,
} from "./fixtures/surface-controls-consumer";
import { classes, classesOf, render, rendered } from "./mount";

function heading(element: HTMLElement): HTMLElement {
  return element.querySelector("h1, h2, h3, h4, h5, h6") as HTMLElement;
}

describe("the Base SectionHeading", () => {
  it("keeps visual size independent from document-outline depth", () => {
    expect(SECTION_HEADING_LEVELS).toEqual([1, 2, 3, 4, 5, 6]);
    expect(SECTION_HEADING_SIZES).toEqual(["sm", "md", "lg", "display"]);

    for (const size of SECTION_HEADING_SIZES) {
      const shallow = heading(rendered(render(SectionHeading, { title: "Deployments", level: 1, size })));
      const deep = heading(rendered(render(SectionHeading, { title: "Deployments", level: 5, size })));
      expect(shallow.tagName).toBe("H1");
      expect(deep.tagName).toBe("H5");
      expect(classes(shallow)).toEqual(classes(deep));
      expect(classes(deep)).toEqual(classesOf(sectionHeading({ size }).title()));
    }
  });

  it("renders supporting text and caller-owned canonical actions", () => {
    const actions = createRawSnippet(() => ({
      render: () => '<button type="button">Filter</button>',
    }));
    const root = rendered(
      render(SectionHeading, {
        title: "Deployments",
        description: "Recent activity",
        actions,
      }),
    );

    expect(root.querySelector("h2")?.textContent).toBe("Deployments");
    expect(root.querySelector("p")?.textContent).toBe("Recent activity");
    expect(root.querySelector("[data-section-heading-title]")?.textContent).toBe("Deployments");
    expect(root.querySelector("[data-section-heading-description]")?.textContent).toBe("Recent activity");
    expect(root.querySelector("button")?.textContent).toBe("Filter");
  });

  it("uses Density roles and inherits every nested appearance axis", () => {
    const root = render(SurfaceControlsConsumer)
      .querySelector<HTMLElement>("[data-section-heading]")!;

    expect(classes(root).has("gap-[var(--reddb-spatial-gap-lg)]")).toBe(true);
    expect(root.hasAttribute("data-theme")).toBe(false);
    expect(root.hasAttribute("data-color-scheme")).toBe(false);
    expect(root.hasAttribute("data-density")).toBe(false);
  });
});
