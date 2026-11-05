// Issue 483: an injected navigation seam. Every in-app link inside a
// NavigationScope — whichever component rendered it — activates through the
// consumer's router; the browser keeps what is the user's or the browser's call.
import { describe, expect, it, vi } from "vitest";
import NavigationScopeConsumer from "./fixtures/NavigationScopeConsumer.svelte";
import { render } from "./mount";

function click(element: Element, init: MouseEventInit = {}): MouseEvent {
  const event = new MouseEvent("click", { bubbles: true, cancelable: true, button: 0, ...init });
  element.dispatchEvent(event);
  return event;
}

describe("the Base NavigationScope", () => {
  it("routes every in-app link inside it through the injected seam, whichever component rendered it", () => {
    const navigate = vi.fn();
    const root = render(NavigationScopeConsumer, { navigate });
    const inApp = [
      root.querySelector('[data-probe="link"]')!,
      root.querySelector('nav a[href="/databases/main"]')!,
      root.querySelector('a[href="/rows?page=2"]')!,
      root.querySelector('[data-probe="caller"]')!,
    ];
    for (const anchor of inApp) {
      const event = click(anchor);
      expect(event.defaultPrevented, anchor.getAttribute("href")!).toBe(true);
    }
    expect(navigate.mock.calls.map(([href]) => href)).toEqual([
      "/collections",
      "/databases/main",
      "/rows?page=2",
      "/caller-owned",
    ]);
    expect(navigate.mock.calls[0]![1]).toBeInstanceOf(MouseEvent);
  });

  it("leaves to the browser what is the user's or the browser's call", () => {
    const navigate = vi.fn();
    const root = render(NavigationScopeConsumer, { navigate });
    const caller = root.querySelector('[data-probe="caller"]')!;
    for (const init of [{ metaKey: true }, { ctrlKey: true }, { shiftKey: true }, { altKey: true }, { button: 1 }]) {
      expect(click(caller, init).defaultPrevented, JSON.stringify(init)).toBe(false);
    }
    for (const probe of ["external", "download", "blank", "fragment", "opt-out"]) {
      expect(click(root.querySelector(`[data-probe="${probe}"]`)!).defaultPrevented, probe).toBe(false);
    }
    expect(navigate).not.toHaveBeenCalled();
  });

  it("changes nothing without a scope: links stay native anchors", () => {
    const navigate = vi.fn();
    const root = render(NavigationScopeConsumer, { navigate, scoped: false });
    expect(click(root.querySelector('[data-probe="link"]')!).defaultPrevented).toBe(false);
    expect(root.querySelector("[data-navigation-scope]")).toBeNull();
    expect(navigate).not.toHaveBeenCalled();
  });

  it("renders no box of its own", () => {
    const root = render(NavigationScopeConsumer, { navigate: vi.fn() });
    expect(root.querySelector("[data-navigation-scope]")!.classList.contains("contents")).toBe(true);
  });
});
