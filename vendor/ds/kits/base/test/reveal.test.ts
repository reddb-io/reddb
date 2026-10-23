import { createRawSnippet, flushSync } from "svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Reveal } from "@reddb-io/design-system/base";
import RevealAppearanceConsumer from "./fixtures/RevealAppearanceConsumer.svelte";
import { render, rendered } from "./mount";

function mockReducedMotion(initial: boolean) {
  let matches = initial;
  const listeners = new Set<(event: MediaQueryListEvent) => void>();
  const media = {
    get matches() {
      return matches;
    },
    media: "(prefers-reduced-motion: reduce)",
    onchange: null,
    addEventListener: vi.fn((_type: string, listener: (event: MediaQueryListEvent) => void) => {
      listeners.add(listener);
    }),
    removeEventListener: vi.fn((_type: string, listener: (event: MediaQueryListEvent) => void) => {
      listeners.delete(listener);
    }),
  } as unknown as MediaQueryList;
  vi.stubGlobal("matchMedia", vi.fn(() => media));

  return {
    set(next: boolean) {
      matches = next;
      for (const listener of listeners) {
        listener({ matches: next, media: media.media } as MediaQueryListEvent);
      }
      flushSync();
    },
  };
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("the Base Reveal Motion Primitive", () => {
  it("reveals content with sane zero-config defaults on the native CSS path", () => {
    vi.stubGlobal("CSS", { supports: vi.fn(() => true) });

    const root = rendered(
      render(Reveal, {
        children: createRawSnippet(() => ({ render: () => "<p>Ready</p>" })),
      }),
    );

    expect(root.getAttribute("data-reveal-path")).toBe("css");
    expect(root.getAttribute("data-reveal-direction")).toBe("up");
    expect(root.getAttribute("data-reveal-delay")).toBe("0");
    expect(root.hasAttribute("data-reveal-debug")).toBe(false);
    expect(root.textContent).toBe("Ready");
  });

  it("accepts direction and delay options and exposes its debug range", () => {
    vi.stubGlobal("CSS", { supports: vi.fn(() => true) });

    const root = rendered(render(Reveal, { direction: "left", delay: 180, debug: true }));

    expect(root.getAttribute("data-reveal-direction")).toBe("left");
    expect(root.style.getPropertyValue("--reveal-delay")).toBe("180ms");
    expect(root.hasAttribute("data-reveal-debug")).toBe(true);
  });

  it("reactively drops positional movement under reduced motion on the CSS path", () => {
    vi.stubGlobal("CSS", { supports: vi.fn(() => true) });
    const preference = mockReducedMotion(true);

    const root = rendered(render(Reveal, { direction: "right" }));

    expect(root.getAttribute("data-reveal-motion")).toBe("fade");
    expect(root.style.getPropertyValue("--reveal-x")).toBe("0px");
    expect(root.style.getPropertyValue("--reveal-y")).toBe("0px");
    expect(root.hasAttribute("data-reveal-entered")).toBe(true);
    expect(root.classList).toContain("opacity-100");
    expect(root.classList).not.toContain("opacity-0");
    expect(root.classList).toContain("motion-reduce:!opacity-100");
    expect(root.classList).toContain("motion-reduce:!transform-none");

    preference.set(false);
    expect(root.getAttribute("data-reveal-motion")).toBe("position-and-fade");
    expect(root.style.getPropertyValue("--reveal-x")).toBe("");
    expect(root.style.getPropertyValue("--reveal-y")).toBe("");
  });

  it("reveals on intersection when scroll-driven CSS is unavailable", () => {
    vi.stubGlobal("CSS", { supports: vi.fn(() => false) });
    mockReducedMotion(false);
    let enter: ((entries: IntersectionObserverEntry[]) => void) | undefined;
    const observe = vi.fn();
    const disconnect = vi.fn();
    vi.stubGlobal(
      "IntersectionObserver",
      // A constructor mock: Vitest only lets `new` call a `function` or class
      // implementation, never an arrow function.
      vi.fn(function (callback: (entries: IntersectionObserverEntry[]) => void) {
        enter = callback;
        return { observe, disconnect };
      }),
    );

    const root = rendered(render(Reveal));
    expect(root.getAttribute("data-reveal-path")).toBe("js");
    expect(root.hasAttribute("data-reveal-entered")).toBe(false);
    expect(observe).toHaveBeenCalledWith(root);

    enter?.([{ isIntersecting: true } as IntersectionObserverEntry]);
    flushSync();

    expect(root.hasAttribute("data-reveal-entered")).toBe(true);
    expect(disconnect).toHaveBeenCalledOnce();
  });

  it("drops positional movement and the observer under reduced motion on the JS path", () => {
    vi.stubGlobal("CSS", { supports: vi.fn(() => false) });
    mockReducedMotion(true);
    const observe = vi.fn();
    const IntersectionObserverMock = vi.fn(() => ({ observe, disconnect: vi.fn() }));
    vi.stubGlobal("IntersectionObserver", IntersectionObserverMock);

    const root = rendered(render(Reveal, { direction: "down" }));

    expect(root.getAttribute("data-reveal-path")).toBe("js");
    expect(root.getAttribute("data-reveal-motion")).toBe("fade");
    expect(root.style.getPropertyValue("--reveal-x")).toBe("0px");
    expect(root.style.getPropertyValue("--reveal-y")).toBe("0px");
    expect(root.hasAttribute("data-reveal-entered")).toBe(true);
    expect(IntersectionObserverMock).not.toHaveBeenCalled();
    expect(observe).not.toHaveBeenCalled();
  });

  it("renders cleanly across Theme × Color Scheme × Density", () => {
    vi.stubGlobal("CSS", { supports: vi.fn(() => true) });
    mockReducedMotion(false);

    const cells = render(RevealAppearanceConsumer).querySelectorAll("[data-reveal]");

    expect(cells).toHaveLength(16);
    for (const cell of cells) {
      expect(cell.textContent).toBe("Visible in every appearance");
      expect(cell.hasAttribute("data-theme")).toBe(false);
      expect(cell.hasAttribute("data-color-scheme")).toBe(false);
      expect(cell.hasAttribute("data-density")).toBe(false);
    }
  });
});
