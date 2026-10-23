import { createRawSnippet, flushSync } from "svelte";
import { describe, expect, it, vi } from "vitest";
import { Carousel } from "./fixtures/media-presentation-consumer";
import { render, rendered } from "./mount";

const slides = ["Architecture", "Delivery", "Operations"].map((label) => ({
  label,
  content: createRawSnippet(() => ({ render: () => `<p>${label}</p>` })),
}));

describe("the Base Carousel", () => {
  it("names its slide set and moves through it with canonical controls", () => {
    const element = rendered(render(Carousel, {
      label: "Platform capabilities",
      slides,
    }));
    const panels = [...element.querySelectorAll<HTMLElement>("[data-carousel-slide]")];
    const next = element.querySelector<HTMLButtonElement>("[aria-label='Next slide']")!;

    expect(element.getAttribute("role")).toBe("region");
    expect(element.getAttribute("aria-roledescription")).toBe("carousel");
    expect(element.getAttribute("aria-label")).toBe("Platform capabilities");
    expect(panels.map((panel) => panel.hidden)).toEqual([false, true, true]);
    expect(panels[0]?.getAttribute("aria-label")).toBe("1 of 3 — Architecture");

    next.focus();
    next.click();
    flushSync();
    expect(document.activeElement).toBe(next);
    expect(panels.map((panel) => panel.hidden)).toEqual([true, false, true]);
  });

  it("makes opt-in rotation explicitly pausable", () => {
    vi.useFakeTimers();
    try {
      const element = rendered(render(Carousel, {
        label: "Platform capabilities",
        slides,
        autoplay: true,
        interval: 1_000,
      }));
      const panels = [...element.querySelectorAll<HTMLElement>("[data-carousel-slide]")];
      const viewport = element.querySelector<HTMLElement>("[data-carousel-viewport]")!;
      const rotation = element.querySelector<HTMLButtonElement>("[data-carousel-rotation]")!;

      expect(rotation.getAttribute("aria-label")).toBe("Pause slide rotation");
      expect(viewport.getAttribute("aria-live")).toBe("off");
      vi.advanceTimersByTime(1_000);
      flushSync();
      expect(panels.map((panel) => panel.hidden)).toEqual([true, false, true]);

      rotation.click();
      flushSync();
      expect(rotation.getAttribute("aria-label")).toBe("Resume slide rotation");
      expect(viewport.getAttribute("aria-live")).toBe("polite");
      vi.advanceTimersByTime(2_000);
      flushSync();
      expect(panels.map((panel) => panel.hidden)).toEqual([true, false, true]);
    } finally {
      vi.useRealTimers();
    }
  });
});
