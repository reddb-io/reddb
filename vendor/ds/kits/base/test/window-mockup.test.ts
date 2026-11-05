import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import {
  DEFAULT_WINDOW_MOCKUP_RATIO,
  WindowMockup,
  windowMockup as windowMockupAppearance,
} from "../src/index";
import { classes, classesOf, render, rendered, styleOf } from "./mount";

const buildLogs = () =>
  createRawSnippet(() => ({
    render: () =>
      '<section aria-label="Build logs"><pre tabindex="0">pnpm test\nall green</pre></section>',
  }));

describe("the Base WindowMockup", () => {
  it("frames caller content without adding a named window or focus stop", () => {
    const element = rendered(
      render(WindowMockup, {
        title: "Terminal",
        ratio: 2,
        class: "max-w-3xl",
        children: buildLogs(),
      }),
    );
    const chrome = element.querySelector<HTMLElement>("[data-window-mockup-chrome]")!;
    const viewport = element.querySelector<HTMLElement>("[data-window-mockup-viewport]")!;
    const section = viewport.querySelector<HTMLElement>('section[aria-label="Build logs"]')!;
    const code = section.querySelector<HTMLElement>("pre")!;

    expect(DEFAULT_WINDOW_MOCKUP_RATIO).toBe(16 / 9);
    expect(element.getAttribute("role")).toBe("presentation");
    expect(element.hasAttribute("aria-label")).toBe(false);
    expect(element.hasAttribute("aria-hidden")).toBe(false);
    expect(chrome.getAttribute("aria-hidden")).toBe("true");
    expect(chrome.textContent).toContain("Terminal");
    expect(chrome.querySelector("a, button, input, select, textarea")).toBeNull();
    expect(section.getAttribute("aria-label")).toBe("Build logs");
    code.focus();
    expect(document.activeElement).toBe(code);
    expect(styleOf(viewport, "aspect-ratio")).toBe("2 / 1");
    expect(classes(element)).toEqual(
      classesOf(windowMockupAppearance().root({ class: "max-w-3xl" })),
    );
    expect(element.hasAttribute("data-theme")).toBe(false);
    expect(element.hasAttribute("data-color-scheme")).toBe(false);
    expect(element.hasAttribute("data-density")).toBe(false);
  });
});
