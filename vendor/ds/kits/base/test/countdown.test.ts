import { describe, expect, it } from "vitest";
import NumericChronologyContractFailures from "./fixtures/NumericChronologyContractFailures.svelte";
import NumericChronologyConsumer from "./fixtures/NumericChronologyConsumer.svelte";
import {
  Countdown,
  countdown as countdownAppearance,
  formatDuration,
  normalizeRemaining,
} from "./fixtures/numeric-chronology-consumer";
import { classes, classesOf, render, rendered } from "./mount";

describe("the deliberately failing Countdown fixture", () => {
  it("demonstrates completion that is never announced", () => {
    const element = rendered(render(NumericChronologyContractFailures, {
      failure: "silent-countdown",
    }));

    expect(element.textContent).toContain("Complete");
    expect(element.getAttribute("role")).not.toBe("status");
    expect(element.hasAttribute("aria-live")).toBe(false);
  });
});

describe("the Base Countdown", () => {
  it("normalizes and exposes a running duration without a noisy live region", () => {
    const element = rendered(render(Countdown, {
      label: "Deployment window",
      remaining: 65.8,
    }));

    expect(element.getAttribute("role")).toBe("timer");
    expect(element.getAttribute("aria-live")).toBe("off");
    expect(element.getAttribute("data-state")).toBe("running");
    expect(element.querySelector("[data-countdown-label]")?.textContent).toBe("Deployment window");
    const time = element.querySelector<HTMLTimeElement>("time")!;
    expect(time.dateTime).toBe("PT1M5S");
    expect(time.textContent).toBe("1:05");
    expect(normalizeRemaining(Number.NaN)).toBe(0);
    expect(formatDuration(3661)).toBe("1:01:01");
  });

  it("turns completion into one atomic polite status", () => {
    const element = rendered(render(Countdown, {
      label: "Deployment window",
      remaining: -1,
      completeLabel: "Window closed",
    }));

    expect(element.getAttribute("role")).toBe("status");
    expect(element.getAttribute("aria-live")).toBe("polite");
    expect(element.getAttribute("aria-atomic")).toBe("true");
    expect(element.getAttribute("data-state")).toBe("complete");
    expect(element.textContent).toContain("Deployment window");
    expect(element.textContent).toContain("Window closed");
  });

  it("inherits nested appearance and Density", () => {
    const scope = render(NumericChronologyConsumer)
      .querySelector<HTMLElement>("[data-numeric-chronology-scope]")!;
    const element = scope.querySelector<HTMLElement>("[data-countdown]")!;

    expect(classes(element)).toEqual(classesOf(countdownAppearance().root()));
    expect(classes(element).has("gap-[var(--reddb-spatial-gap-sm)]")).toBe(true);
    expect(scope.getAttribute("data-density")).toBe("compact");
    expect(element.hasAttribute("data-theme")).toBe(false);
    expect(element.hasAttribute("data-color-scheme")).toBe(false);
    expect(element.hasAttribute("data-contrast")).toBe(false);
    expect(element.hasAttribute("data-density")).toBe(false);
  });
});
