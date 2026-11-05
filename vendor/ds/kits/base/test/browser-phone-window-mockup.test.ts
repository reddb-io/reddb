import { describe, expect, it } from "vitest";
import {
  BrowserMockup,
  PhoneMockup,
  WindowMockup,
  browserMockup,
  phoneMockup,
  windowMockup,
} from "./fixtures/mockup-consumer";
import MockupConsumer from "./fixtures/MockupConsumer.svelte";
import { render } from "./mount";

describe("the Base mockup consumer seam", () => {
  it("exports every frame and appearance function from the installed Base subpath", () => {
    expect([BrowserMockup, PhoneMockup, WindowMockup]).toHaveLength(3);
    expect([browserMockup, phoneMockup, windowMockup].every((appearance) =>
      typeof appearance === "function"
    )).toBe(true);
  });

  it("preserves nested appearance and the caller's semantic focus order", () => {
    const root = render(MockupConsumer);
    const scope = root.querySelector<HTMLElement>("[data-mockup-appearance-scope]")!;
    const frames = scope.querySelectorAll<HTMLElement>(
      "[data-browser-mockup], [data-phone-mockup], [data-window-mockup]",
    );
    const chrome = scope.querySelectorAll<HTMLElement>(
      "[data-browser-mockup-chrome], [data-phone-mockup-chrome], [data-window-mockup-chrome]",
    );
    const controls = scope.querySelectorAll<HTMLElement>("button, a[href], [tabindex='0']");

    expect(scope.getAttribute("data-theme")).toBe("marketing");
    expect(scope.getAttribute("data-color-scheme")).toBe("dark");
    expect(scope.getAttribute("data-density")).toBe("compact");
    expect(frames).toHaveLength(3);
    for (const frame of frames) {
      expect(frame.getAttribute("role")).toBe("presentation");
      expect(frame.hasAttribute("aria-hidden")).toBe(false);
      expect(frame.hasAttribute("data-theme")).toBe(false);
      expect(frame.hasAttribute("data-color-scheme")).toBe(false);
      expect(frame.hasAttribute("data-density")).toBe(false);
      expect(frame.tabIndex).toBe(-1);
    }
    for (const decoration of chrome) {
      expect(decoration.getAttribute("aria-hidden")).toBe("true");
      expect(decoration.querySelector("a, button, input, select, textarea")).toBeNull();
    }

    expect(scope.querySelector('article[aria-label="Browser deployment"]')).not.toBeNull();
    expect(scope.querySelector('nav[aria-label="Phone queue"]')).not.toBeNull();
    expect(scope.querySelector('section[aria-label="Window logs"]')).not.toBeNull();
    expect(controls).toHaveLength(3);
    for (const control of controls) {
      control.focus();
      expect(document.activeElement).toBe(control);
    }
  });
});
