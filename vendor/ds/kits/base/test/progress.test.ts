import { describe, expect, it } from "vitest";
import {
  Progress,
  isDeterminate,
  normalizeRange,
  progress as progressAppearance,
} from "./fixtures/progress-feedback-consumer";
import ProgressFeedbackConsumer from "./fixtures/ProgressFeedbackConsumer.svelte";
import { classes, classesOf, render, rendered } from "./mount";

describe("the Base Progress", () => {
  it("announces and draws the same normalized determinate value", () => {
    const element = rendered(
      render(Progress, { value: 125, min: 0, max: 100, label: "Deployment progress" }),
    );
    const indicator = element.querySelector<HTMLElement>("[data-progress-indicator]")!;

    expect(element.getAttribute("role")).toBe("progressbar");
    expect(element.getAttribute("aria-label")).toBe("Deployment progress");
    expect(element.getAttribute("aria-valuenow")).toBe("100");
    expect(element.getAttribute("aria-valuetext")).toBe("100%");
    expect(indicator.style.width).toBe("100%");
  });

  it("represents indeterminate work without inventing a current value", () => {
    const element = rendered(render(Progress, { label: "Preparing deployment" }));
    const indicator = element.querySelector<HTMLElement>("[data-progress-indicator]")!;

    expect(element.getAttribute("data-state")).toBe("indeterminate");
    expect(element.hasAttribute("aria-valuenow")).toBe(false);
    expect(element.getAttribute("aria-valuetext")).toBe("In progress");
    expect(classes(indicator).has("motion-safe:animate-pulse")).toBe(true);
    expect([...classes(indicator)].some((name) => name.startsWith("animate-"))).toBe(false);
  });

  it("can expose the accessible label and value as a visible summary", () => {
    const determinate = rendered(render(Progress, {
      value: 72,
      label: "Deployment progress",
      summary: true,
    }));
    expect(determinate.querySelector("[data-progress-summary]")?.textContent).toContain("Deployment progress");
    expect(determinate.querySelector("[data-progress-summary]")?.textContent).toContain("72%");

    const indeterminate = rendered(render(Progress, {
      label: "Provisioning database",
      summary: true,
    }));
    expect(indeterminate.querySelector("[data-progress-summary]")?.textContent).toContain("In progress");
  });

  it("shares one clamping contract with the other numeric feedback", () => {
    expect(isDeterminate(0)).toBe(true);
    expect(isDeterminate(undefined)).toBe(false);
    expect(normalizeRange(-5, 0, 10)).toEqual({ min: 0, max: 10, value: 0, percent: 0 });
    expect(normalizeRange(5, 10, 10)).toEqual({ min: 10, max: 11, value: 10, percent: 0 });
  });

  it("wears its extension seam and inherits nested appearance and Density", () => {
    const root = render(ProgressFeedbackConsumer);
    const scope = root.querySelector<HTMLElement>("[data-progress-feedback-scope]")!;
    const element = scope.querySelector<HTMLElement>("[data-progress]")!;

    expect(classes(element)).toEqual(classesOf(progressAppearance().root()));
    expect(scope.getAttribute("data-density")).toBe("compact");
    expect(element.hasAttribute("data-theme")).toBe(false);
    expect(element.hasAttribute("data-color-scheme")).toBe(false);
    expect(element.hasAttribute("data-density")).toBe(false);
  });
});
