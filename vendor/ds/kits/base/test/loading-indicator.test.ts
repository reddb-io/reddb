import { describe, expect, it } from "vitest";
import {
  LOADING_INDICATOR_SIZES,
  LoadingIndicator,
  loadingIndicator as loadingIndicatorAppearance,
} from "./fixtures/progress-feedback-consumer";
import ProgressFeedbackConsumer from "./fixtures/ProgressFeedbackConsumer.svelte";
import { classes, classesOf, render, rendered } from "./mount";

describe("the Base LoadingIndicator", () => {
  it("announces a polite busy status with a useful default sentence", () => {
    const element = rendered(render(LoadingIndicator, {}));

    expect(element.getAttribute("role")).toBe("status");
    expect(element.getAttribute("aria-live")).toBe("polite");
    expect(element.getAttribute("aria-busy")).toBe("true");
    expect(element.textContent?.trim()).toBe("Loading…");
    expect(element.querySelector("svg")!.getAttribute("aria-hidden")).toBe("true");
  });

  it("keeps a visually hidden label available to assistive technology", () => {
    const element = rendered(
      render(LoadingIndicator, { label: "Loading nodes…", labelHidden: true }),
    );
    const label = element.querySelector("span")!;

    expect(label.textContent).toBe("Loading nodes…");
    expect(classes(label).has("sr-only")).toBe(true);
  });

  it("animates only behind platform motion preferences", () => {
    for (const size of LOADING_INDICATOR_SIZES) {
      const spinner = rendered(render(LoadingIndicator, { size })).querySelector("svg")!;
      expect(classes(spinner).has("motion-safe:animate-spin")).toBe(true);
      expect(classes(spinner).has("motion-reduce:animate-pulse")).toBe(true);
      expect([...classes(spinner)].some((name) => name.startsWith("animate-"))).toBe(false);
    }
  });

  it("sizes the spinner on the Icon size role of the same name", () => {
    for (const size of LOADING_INDICATOR_SIZES) {
      const spinner = rendered(render(LoadingIndicator, { size })).querySelector("svg")!;
      expect(classes(spinner).has(`size-[var(--reddb-spatial-icon-size-${size})]`), size).toBe(true);
    }
  });

  it("inherits nested appearance and stays outside the keyboard order", () => {
    const root = render(ProgressFeedbackConsumer);
    const element = root.querySelector<HTMLElement>("[data-loading-indicator]")!;

    expect(classes(element)).toEqual(classesOf(loadingIndicatorAppearance().root()));
    expect(element.tabIndex).toBe(-1);
    expect(element.hasAttribute("data-theme")).toBe(false);
    expect(element.hasAttribute("data-color-scheme")).toBe(false);
    expect(element.hasAttribute("data-density")).toBe(false);
  });
});
