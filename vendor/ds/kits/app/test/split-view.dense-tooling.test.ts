// SplitView as dense tooling composes it: drags cross pane contents and nested
// separators remain independent keyboard controls.

import { flushSync } from "svelte";
import { describe, expect, it } from "vitest";
import SplitView from "../src/primitives/SplitView.svelte";
import NestedSplitViewConsumer from "./fixtures/NestedSplitViewConsumer.svelte";
import { render, rendered } from "./mount";

/** The start pane, divider, and end pane, in document order. */
function parts(element: HTMLElement): [HTMLElement, HTMLElement, HTMLElement] {
  const [start, divider, end] = [...element.children] as HTMLElement[];
  return [start!, divider!, end!];
}

/** Give the split a measurable box, which jsdom does not provide. */
function measured(element: HTMLElement, width: number, height: number): void {
  element.getBoundingClientRect = () =>
    ({ left: 0, top: 0, right: width, bottom: height, width, height, x: 0, y: 0 }) as DOMRect;
}

function pointer(element: HTMLElement, type: string, position: Partial<MouseEventInit> = {}): void {
  element.dispatchEvent(new MouseEvent(type, { bubbles: true, cancelable: true, ...position }));
  flushSync();
}

function press(element: HTMLElement, key: string): void {
  element.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }));
  flushSync();
}

describe("SplitView dense-tooling contract", () => {
  it("keeps dragging when the pointer crosses pane content", () => {
    const split = rendered(render(SplitView, {}));
    const [start, divider, end] = parts(split);
    measured(split, 200, 100);

    pointer(divider, "pointerdown", { clientX: 100 });
    pointer(end, "pointermove", { clientX: 150 });

    expect(start.style.flexBasis).toBe("75%");
    expect(end.style.flexBasis).toBe("25%");
  });

  it("composes nested panes whose dividers remain independent keyboard controls", () => {
    const outer = rendered(render(NestedSplitViewConsumer, {}));
    const separators = [...outer.querySelectorAll<HTMLElement>('[role="separator"]')];
    const [outerDivider, innerDivider] = separators;
    const inner = innerDivider?.parentElement as HTMLElement;

    expect(separators.map((separator) => separator.getAttribute("aria-label"))).toEqual([
      "Resize navigation and workspace",
      "Resize editor and console",
    ]);
    expect(outer.getAttribute("data-orientation")).toBe("horizontal");
    expect(inner.getAttribute("data-orientation")).toBe("vertical");

    press(innerDivider!, "ArrowDown");
    expect(parts(inner)[0].style.flexBasis).toBe("62%");
    expect(parts(outer)[0].style.flexBasis).toBe("40%");

    press(outerDivider!, "ArrowRight");
    expect(parts(outer)[0].style.flexBasis).toBe("42%");
    expect(parts(inner)[0].style.flexBasis).toBe("62%");
  });
});
