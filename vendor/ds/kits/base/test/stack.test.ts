import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import {
  STACK_ALIGNMENTS,
  STACK_DIRECTIONS,
  STACK_GAPS,
  STACK_JUSTIFICATIONS,
  Stack,
  stack as stackAppearance,
  type StackGap,
} from "./fixtures/layout-consumer";
import LayoutConsumer from "./fixtures/LayoutConsumer.svelte";
import { classes, classesOf, render, rendered } from "./mount";

const GAP_CLASS: Record<StackGap, string> = {
  none: "gap-0",
  sm: "gap-[var(--reddb-spatial-gap-sm)]",
  md: "gap-[var(--reddb-spatial-gap-md)]",
  lg: "gap-[var(--reddb-spatial-gap-lg)]",
  "layout-sm": "gap-[var(--reddb-spatial-layout-gap-sm)]",
  "layout-md": "gap-[var(--reddb-spatial-layout-gap-md)]",
  "layout-lg": "gap-[var(--reddb-spatial-layout-gap-lg)]",
  section: "gap-[var(--reddb-spatial-section-gap)]",
};

describe("the Base Stack", () => {
  it("offers exactly the token-backed flow gaps", () => {
    // Component, layout and section tiers (ADR 0024).
    expect(STACK_GAPS).toEqual(["none", "sm", "md", "lg", "layout-sm", "layout-md", "layout-lg", "section"]);
    for (const gap of STACK_GAPS) {
      const element = rendered(render(Stack, { gap }));
      expect(classes(element)).toEqual(classesOf(stackAppearance({ gap })));
      expect(classes(element).has(GAP_CLASS[gap])).toBe(true);
    }
  });

  it("defaults to the medium layout gap and preserves child order", () => {
    const element = rendered(
      render(Stack, {
        "aria-label": "Ordered steps",
        class: "min-w-0",
        children: createRawSnippet(() => ({
          render: () => "<div><span>First</span><span>Second</span></div>",
        })),
      }),
    );

    expect(element.getAttribute("aria-label")).toBe("Ordered steps");
    expect(classes(element)).toEqual(
      classesOf(stackAppearance({ gap: "layout-md", class: "min-w-0" })),
    );
    expect(
      [...(element.firstElementChild?.children ?? [])].map((child) => child.textContent),
    ).toEqual(["First", "Second"]);
  });

  it("keeps its public gap while a nested Density scope re-resolves it", () => {
    const root = render(LayoutConsumer);
    const outer = root.querySelector<HTMLElement>('[aria-label="Deployment flow"]')!;
    const inner = root.querySelector<HTMLElement>('[data-density="compact"] [data-stack]')!;

    expect(classes(outer).has(GAP_CLASS.lg)).toBe(true);
    expect(classes(inner).has(GAP_CLASS.sm)).toBe(true);
    expect(outer.hasAttribute("data-density")).toBe(false);
    expect(inner.hasAttribute("data-density")).toBe(false);
  });

  it("owns its direction, alignment and wrapping, so a caller never overrides the flow", () => {
    expect(STACK_DIRECTIONS).toEqual(["column", "row"]);
    expect(STACK_ALIGNMENTS).toEqual(["stretch", "start", "center", "end", "baseline"]);
    expect(STACK_JUSTIFICATIONS).toEqual(["start", "center", "end", "between"]);

    const column = rendered(render(Stack, {}));
    expect(classes(column)).toEqual(classesOf("flex flex-col gap-[var(--reddb-spatial-layout-gap-md)]"));
    expect(column.dataset.stackDirection).toBe("column");

    const row = rendered(
      render(Stack, { direction: "row", align: "baseline", justify: "between", wrap: true, gap: "layout-sm" }),
    );
    expect(classes(row)).toEqual(
      classesOf(
        "flex flex-row items-baseline justify-between flex-wrap gap-[var(--reddb-spatial-layout-gap-sm)]",
      ),
    );
    expect(row.dataset.stackDirection).toBe("row");
  });
});
