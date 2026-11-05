import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import MultiColumnLayout from "../src/composites/MultiColumnLayout.svelte";
import { multiColumnLayout } from "../src/composites/multi-column-layout.variants";
import ApplicationLayoutsConsumer from "./fixtures/ApplicationLayoutsConsumer.svelte";
import { expectLayoutReadiness } from "./layout-readiness";
import { classes, classesOf, render, rendered } from "./mount";

const snippet = (html: string) => createRawSnippet(() => ({ render: () => html }));

describe("MultiColumnLayout", () => {
  it("arranges two named columns without taking ownership of their content", () => {
    const layout = rendered(
      render(MultiColumnLayout, {
        start: snippet("<p>Filters</p>"),
        children: snippet("<article>Deployments</article>"),
      }),
    );

    expect(layout.dataset.columns).toBe("two");
    expect([...layout.children].map((column) => column.getAttribute("data-column"))).toEqual([
      "start",
      "main",
    ]);
    expect(layout.querySelector('[data-column="start"]')?.textContent).toBe("Filters");
    expect(layout.querySelector('[data-column="main"] article')?.textContent).toBe(
      "Deployments",
    );
  });

  it("adds the optional end column as the canonical three-column arrangement", () => {
    const layout = rendered(
      render(MultiColumnLayout, {
        start: snippet("<p>Navigation</p>"),
        children: snippet("<p>Workspace</p>"),
        end: snippet("<p>Inspector</p>"),
      }),
    );

    expect(layout.dataset.columns).toBe("three");
    expect([...layout.children].map((column) => column.getAttribute("data-column"))).toEqual([
      "start",
      "main",
      "end",
    ]);
    expect(classes(layout)).toEqual(classesOf(multiColumnLayout({ columns: "three" }).root()));
  });

  it("preserves document and keyboard order across the visual columns", () => {
    const layout = rendered(
      render(MultiColumnLayout, {
        start: snippet('<button type="button">First</button>'),
        children: snippet('<button type="button">Second</button>'),
        end: snippet('<button type="button">Third</button>'),
      }),
    );

    expect([...layout.querySelectorAll("button")].map((button) => button.textContent)).toEqual([
      "First",
      "Second",
      "Third",
    ]);
    expect([...layout.querySelectorAll("button")].every((button) => button.tabIndex === 0)).toBe(
      true,
    );
  });

  it("composes canonical Stacks and leaves Density live in every column", () => {
    const layout = rendered(
      render(MultiColumnLayout, {
        gap: "sm",
        start: snippet("<p>One</p>"),
        children: snippet("<p>Two</p>"),
        end: snippet("<p>Three</p>"),
      }),
    );

    expect(classes(layout).has("gap-[var(--reddb-spatial-layout-gap-md)]")).toBe(true);
    for (const column of layout.querySelectorAll<HTMLElement>("[data-column]")) {
      expect(column.hasAttribute("data-stack")).toBe(true);
      expect(classes(column).has("gap-[var(--reddb-spatial-gap-sm)]")).toBe(true);
      expect(column.hasAttribute("data-density")).toBe(false);
    }
  });

  it("passes native attributes, merges classes, and inherits nested appearance", () => {
    const layout = rendered(
      render(MultiColumnLayout, { id: "columns", class: "relative" }),
    );
    expect(layout.id).toBe("columns");
    expect(classes(layout)).toEqual(
      classesOf(multiColumnLayout({ columns: "two" }).root({ class: "relative" })),
    );

    const consumer = render(ApplicationLayoutsConsumer);
    const nested = consumer.querySelector<HTMLElement>("[data-multi-column-layout]")!;
    expect(nested.closest('[data-density="compact"]')).not.toBeNull();
    expect(nested.hasAttribute("data-theme")).toBe(false);
    expect(nested.hasAttribute("data-color-scheme")).toBe(false);
    expect(nested.hasAttribute("data-density")).toBe(false);
  });

  it("ships complete showcase, consumer, distributed export, and readiness evidence", () => {
    expectLayoutReadiness("multi-column-layout", "MultiColumnLayout");
  });
});
