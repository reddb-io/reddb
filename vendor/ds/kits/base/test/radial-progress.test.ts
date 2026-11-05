import { describe, expect, it } from "vitest";
import {
  RADIAL_PROGRESS_SIZES,
  RadialProgress,
  radialProgress as radialProgressAppearance,
} from "./fixtures/progress-feedback-consumer";
import ProgressFeedbackConsumer from "./fixtures/ProgressFeedbackConsumer.svelte";
import { classes, classesOf, render, rendered } from "./mount";

describe("the Base RadialProgress", () => {
  it("uses the linear progress value contract around a non-color arc", () => {
    const element = rendered(
      render(RadialProgress, { value: 64, label: "Indexing progress" }),
    );
    const indicator = element.querySelector<SVGCircleElement>("circle:last-of-type")!;

    expect(element.getAttribute("role")).toBe("progressbar");
    expect(element.getAttribute("aria-valuenow")).toBe("64");
    expect(element.getAttribute("aria-valuetext")).toBe("64%");
    expect(element.querySelector("span")!.textContent).toBe("64%");
    expect(indicator.getAttribute("stroke-dashoffset")).toBe("36");
  });

  it("supports indeterminate radial work without exposing a false value", () => {
    const element = rendered(render(RadialProgress, { label: "Indexing" }));
    const svg = element.querySelector("svg")!;

    expect(element.hasAttribute("aria-valuenow")).toBe(false);
    expect(element.getAttribute("aria-valuetext")).toBe("In progress");
    expect(classes(svg).has("motion-safe:animate-spin")).toBe(true);
    expect([...classes(svg)].some((name) => name.startsWith("animate-"))).toBe(false);
  });

  it("offers a closed graphic size vocabulary", () => {
    expect(RADIAL_PROGRESS_SIZES).toEqual(["sm", "md", "lg"]);
    for (const size of RADIAL_PROGRESS_SIZES) {
      const element = rendered(render(RadialProgress, { value: 50, size }));
      expect(classes(element)).toEqual(
        classesOf(radialProgressAppearance({ size }).root()),
      );
    }
  });

  it("inherits nested appearance without selecting an axis", () => {
    const root = render(ProgressFeedbackConsumer);
    const element = root.querySelector<HTMLElement>("[data-radial-progress]")!;

    expect(element.hasAttribute("data-theme")).toBe(false);
    expect(element.hasAttribute("data-color-scheme")).toBe(false);
    expect(element.hasAttribute("data-density")).toBe(false);
  });
});
