import { describe, expect, it } from "vitest";
import { Meter, meter as meterAppearance } from "./fixtures/progress-feedback-consumer";
import ProgressFeedbackConsumer from "./fixtures/ProgressFeedbackConsumer.svelte";
import { classes, classesOf, render, rendered } from "./mount";

describe("the Base Meter", () => {
  it("keeps its accessible name and reading when a table hides the repeated label", () => {
    const element = rendered(render(Meter, { value: 65.3, label: "Node 07 memory", layout: "inline", labelHidden: true, formatValue: (value: number) => `${value.toFixed(1)}%` }));
    expect(element.getAttribute("aria-label")).toBe("Node 07 memory");
    expect(element.getAttribute("aria-valuetext")).toBe("65.3%");
    expect(element.querySelector("[data-meter-summary] span")?.classList.contains("sr-only")).toBe(true);
    expect(element.querySelector<HTMLElement>("[data-meter-indicator]")!.style.width).toBe("65.3%");
  });
  it("conveys its value through semantics, text, and fill length", () => {
    const element = rendered(render(Meter, { value: 3, max: 5, label: "Cluster capacity" }));
    const indicator = element.querySelector<HTMLElement>("[data-meter-indicator]")!;

    expect(element.getAttribute("role")).toBe("meter");
    expect(element.getAttribute("aria-valuemin")).toBe("0");
    expect(element.getAttribute("aria-valuemax")).toBe("5");
    expect(element.getAttribute("aria-valuenow")).toBe("3");
    expect(element.getAttribute("aria-valuetext")).toBe("60%");
    expect(element.textContent).toContain("Cluster capacity");
    expect(element.textContent).toContain("60%");
    expect(indicator.style.width).toBe("60%");
  });

  it("clamps what it draws and announces to the declared range", () => {
    const element = rendered(
      render(Meter, {
        value: -2,
        min: 1,
        max: 4,
        formatValue: (value: number) => `${value} replicas`,
      }),
    );

    expect(element.getAttribute("aria-valuenow")).toBe("1");
    expect(element.getAttribute("aria-valuetext")).toBe("1 replicas");
    expect(element.querySelector<HTMLElement>("[data-meter-indicator]")!.style.width).toBe("0%");
  });

  it("wears token-backed appearance and inherits every active axis", () => {
    const root = render(ProgressFeedbackConsumer);
    const scope = root.querySelector<HTMLElement>("[data-progress-feedback-scope]")!;
    const element = scope.querySelector<HTMLElement>("[data-meter]")!;

    expect(classes(element)).toEqual(classesOf(meterAppearance().root()));
    expect(scope.getAttribute("data-theme")).toBe("marketing");
    expect(scope.getAttribute("data-color-scheme")).toBe("dark");
    expect(scope.getAttribute("data-density")).toBe("compact");
    expect(element.hasAttribute("data-theme")).toBe(false);
    expect(element.hasAttribute("data-color-scheme")).toBe(false);
    expect(element.hasAttribute("data-density")).toBe(false);
  });

  it("draws its fill in the requested tone and defaults to ink", () => {
    const standard = rendered(render(Meter, { value: 1, max: 2 }));
    const primary = rendered(render(Meter, { value: 1, max: 2, tone: "primary" }));
    const warning = rendered(render(Meter, { value: 1, max: 2, tone: "warning" }));
    const fill = (element: HTMLElement) =>
      classes(element.querySelector<HTMLElement>("[data-meter-indicator]")!);

    expect(fill(standard).has("bg-foreground")).toBe(true);
    expect(fill(standard).has("bg-primary")).toBe(false);
    expect(fill(primary).has("bg-primary")).toBe(true);
    expect(fill(warning).has("bg-feedback-warning-foreground")).toBe(true);
  });
});

describe("the Meter value", () => {
  it("is drawn in ink, not in the Brand red", () => {
    const element = rendered(render(Meter, { value: 3, max: 5, label: "Cluster capacity" }));
    const names = classes(element.querySelector<HTMLElement>("[data-meter-indicator]")!);

    expect(names.has("bg-foreground")).toBe(true);
    expect(names.has("bg-primary")).toBe(false);
  });
});
