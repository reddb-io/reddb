import { describe, expect, it } from "vitest";
import NumericChronologyConsumer from "./fixtures/NumericChronologyConsumer.svelte";
import { Statistic, statistic as statisticAppearance } from "./fixtures/numeric-chronology-consumer";
import { classes, classesOf, render, rendered } from "./mount";

describe("the Base Statistic", () => {
  it("pairs a caller-owned label with a machine-readable formatted value", () => {
    const element = rendered(render(Statistic, {
      label: "Monthly queries",
      value: 12840,
      formatValue: (value) => Number(value).toLocaleString("en-US"),
      description: "Across all clusters",
    }));

    expect(element.tagName).toBe("DL");
    expect(element.querySelector("dt")?.textContent).toContain("Monthly queries");
    const value = element.querySelector<HTMLDataElement>("[data-statistic-value]")!;
    expect(value.value).toBe("12840");
    expect(value.textContent).toBe("12,840");
    expect(element.querySelector("[data-statistic-description]")?.textContent)
      .toBe("Across all clusters");
    expect(element.tabIndex).toBe(-1);
  });

  it("inherits nested appearance and keeps its public appearance seam", () => {
    const scope = render(NumericChronologyConsumer)
      .querySelector<HTMLElement>("[data-numeric-chronology-scope]")!;
    const element = scope.querySelector<HTMLElement>("[data-statistic]")!;

    expect(classes(element)).toEqual(expect.objectContaining(classesOf(statisticAppearance().root())));
    expect(scope.getAttribute("data-density")).toBe("compact");
    expect(element.hasAttribute("data-theme")).toBe(false);
    expect(element.hasAttribute("data-color-scheme")).toBe(false);
    expect(element.hasAttribute("data-contrast")).toBe(false);
    expect(element.hasAttribute("data-density")).toBe(false);
  });
});
