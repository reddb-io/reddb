import { describe, expect, it } from "vitest";
import {
  SKELETON_SHAPES,
  Skeleton,
  skeleton as skeletonAppearance,
} from "./fixtures/progress-feedback-consumer";
import ProgressFeedbackConsumer from "./fixtures/ProgressFeedbackConsumer.svelte";
import { classes, classesOf, render, rendered } from "./mount";

describe("the Base Skeleton", () => {
  it("offers text, rectangle, and circle placeholders without prescribing content", () => {
    expect(SKELETON_SHAPES).toEqual(["text", "rectangle", "circle"]);
    for (const shape of SKELETON_SHAPES) {
      const element = rendered(render(Skeleton, { shape, class: "max-w-md" }));
      expect(element.getAttribute("data-shape")).toBe(shape);
      expect(classes(element)).toEqual(
        classesOf(skeletonAppearance({ shape, class: "max-w-md" })),
      );
    }
  });

  it("is decorative by default and announces busy only when given ownership", () => {
    const decorative = rendered(render(Skeleton, {}));
    const announcing = rendered(render(Skeleton, { label: "Loading node summary" }));

    expect(decorative.getAttribute("aria-hidden")).toBe("true");
    expect(decorative.hasAttribute("role")).toBe(false);
    expect(announcing.getAttribute("role")).toBe("status");
    expect(announcing.getAttribute("aria-live")).toBe("polite");
    expect(announcing.getAttribute("aria-busy")).toBe("true");
    expect(announcing.getAttribute("aria-label")).toBe("Loading node summary");
  });

  it("pulses only when motion is welcome and leaves rectangle height live to Density", () => {
    const rectangle = rendered(render(Skeleton, { shape: "rectangle" }));

    expect(classes(rectangle).has("motion-safe:animate-pulse")).toBe(true);
    expect([...classes(rectangle)].some((name) => name.startsWith("animate-"))).toBe(false);
    expect(classes(rectangle).has("min-h-[var(--reddb-spatial-control-height-lg)]")).toBe(true);
  });

  it("inherits nested appearance without interrupting caller focus order", () => {
    const root = render(ProgressFeedbackConsumer);
    const element = root.querySelector<HTMLElement>("[data-skeleton]")!;
    const buttons = root.querySelectorAll<HTMLButtonElement>("button");

    buttons[0]!.focus();
    expect(document.activeElement).toBe(buttons[0]);
    expect(element.tabIndex).toBe(-1);
    buttons[1]!.focus();
    expect(document.activeElement).toBe(buttons[1]);
    expect(element.hasAttribute("data-theme")).toBe(false);
    expect(element.hasAttribute("data-color-scheme")).toBe(false);
    expect(element.hasAttribute("data-density")).toBe(false);
  });
});
