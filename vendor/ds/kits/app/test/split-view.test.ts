// SplitView, as an application meets it.
//
// The arithmetic is already proven in split-view.behavior.test.ts, so what is
// left here is everything that needs a DOM: which element carries the drag,
// what the separator tells assistive technology, and that the panes really do
// resize — by pointer *and* by keyboard, because a splitter that only answers
// to a mouse is the usual one and is unusable without one.

import { flushSync } from "svelte";
import { describe, expect, it } from "vitest";
import SplitView from "../src/primitives/SplitView.svelte";
import { SPLIT_BOUNDS, SPLIT_STEP } from "../src/primitives/split-view.behavior";
import { splitView } from "../src/primitives/split-view.variants";
import { classes, classesOf, render, rendered, text } from "./mount";

/** The start pane, the divider and the end pane, in document order. */
function parts(element: HTMLElement): [HTMLElement, HTMLElement, HTMLElement] {
  const [start, divider, end] = [...element.children] as HTMLElement[];
  return [start!, divider!, end!];
}

/** Both panes' widths, as the inline style carries them. */
function basis(element: HTMLElement): [string, string] {
  const [start, , end] = parts(element);
  return [start.style.flexBasis, end.style.flexBasis];
}

/** Give the container a size, which jsdom otherwise reports as zero. */
function measured(element: HTMLElement, width: number, height: number): void {
  element.getBoundingClientRect = () =>
    ({ left: 0, top: 0, right: width, bottom: height, width, height, x: 0, y: 0 }) as DOMRect;
}

function press(element: HTMLElement, key: string): KeyboardEvent {
  const event = new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true });
  element.dispatchEvent(event);
  flushSync();
  return event;
}

function pointer(element: HTMLElement, type: string, position: Partial<MouseEventInit> = {}): void {
  element.dispatchEvent(new MouseEvent(type, { bubbles: true, cancelable: true, ...position }));
  flushSync();
}

describe("SplitView", () => {
  it("renders two panes with a divider between them", () => {
    const element = rendered(render(SplitView, { start: text("left"), end: text("right") }));
    const [start, divider, end] = parts(element);
    expect(start.textContent?.trim()).toBe("left");
    expect(divider.getAttribute("role")).toBe("separator");
    expect(end.textContent?.trim()).toBe("right");
    expect(element.getAttribute("data-orientation")).toBe("horizontal");
  });

  it("splits the container down the middle by default", () => {
    expect(basis(rendered(render(SplitView, {})))).toEqual(["50%", "50%"]);
  });

  it("takes the fraction it is given, and keeps both panes on screen", () => {
    expect(basis(rendered(render(SplitView, { fraction: 0.25 })))).toEqual(["25%", "75%"]);
    // A fraction outside the bounds is held at them rather than collapsing a pane.
    expect(basis(rendered(render(SplitView, { fraction: 2 })))).toEqual([
      `${SPLIT_BOUNDS.max * 100}%`,
      `${Math.round((1 - SPLIT_BOUNDS.max) * 1000) / 10}%`,
    ]);
  });

  it("describes the split to assistive technology as a movable separator", () => {
    const [, divider] = parts(rendered(render(SplitView, { fraction: 0.4 })));
    // Panes side by side are divided by a vertical rule.
    expect(divider.getAttribute("aria-orientation")).toBe("vertical");
    expect(divider.getAttribute("aria-valuenow")).toBe("40");
    expect(divider.getAttribute("aria-valuemin")).toBe(`${SPLIT_BOUNDS.min * 100}`);
    expect(divider.getAttribute("aria-valuemax")).toBe(`${SPLIT_BOUNDS.max * 100}`);
    // Reachable without a pointer at all, and named when it gets there.
    expect(divider.getAttribute("tabindex")).toBe("0");
    expect(divider.getAttribute("aria-label")).toBe("Resize panes");
  });

  it("takes the caller's name for the divider", () => {
    const [, divider] = parts(rendered(render(SplitView, { label: "Resize the inspector" })));
    expect(divider.getAttribute("aria-label")).toBe("Resize the inspector");
  });

  it("turns the rule the other way when the panes are stacked", () => {
    const element = rendered(render(SplitView, { orientation: "vertical" }));
    const [start, divider] = parts(element);
    expect(element.getAttribute("data-orientation")).toBe("vertical");
    expect(divider.getAttribute("aria-orientation")).toBe("horizontal");
    expect(classes(element)).toEqual(classesOf(splitView({ orientation: "vertical" }).root()));
    expect(classes(start)).toEqual(classesOf(splitView({ orientation: "vertical" }).pane()));
  });

  it("moves the divider with the arrow keys of its own axis", () => {
    const element = rendered(render(SplitView, {}));
    const [, divider] = parts(element);

    const forward = press(divider, "ArrowRight");
    expect(forward.defaultPrevented).toBe(true);
    expect(basis(element)[0]).toBe(`${(0.5 + SPLIT_STEP) * 100}%`);

    press(divider, "ArrowLeft");
    expect(basis(element)[0]).toBe("50%");
  });

  it("takes the divider to either end", () => {
    const element = rendered(render(SplitView, {}));
    const [, divider] = parts(element);

    press(divider, "End");
    expect(basis(element)[0]).toBe(`${SPLIT_BOUNDS.max * 100}%`);
    press(divider, "Home");
    expect(basis(element)[0]).toBe(`${SPLIT_BOUNDS.min * 100}%`);
  });

  it("leaves the keys that are not its own to the page", () => {
    const element = rendered(render(SplitView, {}));
    const [, divider] = parts(element);

    for (const key of ["ArrowUp", "ArrowDown", "PageDown", "Tab"]) {
      const event = press(divider, key);
      // Not prevented, and the split has not moved: the page keeps its keys.
      expect(event.defaultPrevented).toBe(false);
      expect(basis(element)[0]).toBe("50%");
    }
  });

  it("resizes on a drag, once the divider has been taken hold of", () => {
    const element = rendered(render(SplitView, {}));
    const [, divider] = parts(element);
    measured(element, 200, 100);

    // A move nobody started is somebody else's pointer crossing the divider.
    pointer(divider, "pointermove", { clientX: 40 });
    expect(basis(element)[0]).toBe("50%");

    pointer(divider, "pointerdown", { clientX: 100 });
    pointer(divider, "pointermove", { clientX: 40 });
    expect(basis(element)).toEqual(["20%", "80%"]);
  });

  it("follows the other axis when the panes are stacked", () => {
    const element = rendered(render(SplitView, { orientation: "vertical" }));
    const [, divider] = parts(element);
    measured(element, 200, 100);

    pointer(divider, "pointerdown", { clientY: 50 });
    pointer(divider, "pointermove", { clientY: 30 });
    expect(basis(element)).toEqual(["30%", "70%"]);
  });

  it("stops following the pointer once it is let go", () => {
    const element = rendered(render(SplitView, {}));
    const [, divider] = parts(element);
    measured(element, 200, 100);

    pointer(divider, "pointerdown", { clientX: 100 });
    pointer(divider, "pointermove", { clientX: 60 });
    expect(basis(element)[0]).toBe("30%");

    pointer(divider, "pointerup", { clientX: 60 });
    pointer(divider, "pointermove", { clientX: 160 });
    expect(basis(element)[0]).toBe("30%");
  });

  it("leaves the divider alone in a container it cannot measure", () => {
    // jsdom reports every box as zero, which is also what a hidden container
    // reports in a browser: a drag with nothing to divide must not slam the
    // divider to an edge.
    const element = rendered(render(SplitView, {}));
    const [, divider] = parts(element);

    pointer(divider, "pointerdown", { clientX: 100 });
    pointer(divider, "pointermove", { clientX: 40 });
    expect(basis(element)[0]).toBe("50%");
  });

  it("shows that the divider is being held, and stops when it is not", () => {
    const element = rendered(render(SplitView, {}));
    const [, divider] = parts(element);

    pointer(divider, "pointerdown", { clientX: 100 });
    expect(classes(divider)).toEqual(classesOf(splitView({ dragging: true }).divider()));

    pointer(divider, "pointerup", { clientX: 100 });
    expect(classes(divider)).toEqual(classesOf(splitView({ dragging: false }).divider()));
  });

  it("wears exactly the classes its variants module produces", () => {
    const element = rendered(render(SplitView, {}));
    const [start, divider, end] = parts(element);
    const slots = splitView({ orientation: "horizontal", dragging: false });
    expect(classes(element)).toEqual(classesOf(slots.root()));
    expect(classes(start)).toEqual(classesOf(slots.pane()));
    expect(classes(end)).toEqual(classesOf(slots.pane()));
    expect(classes(divider)).toEqual(classesOf(slots.divider()));
  });

  it("merges a caller's classes over the root slot's own", () => {
    const element = rendered(render(SplitView, { class: "h-64" }));
    expect(classes(element).has("h-64")).toBe(true);
    expect(classes(element).has("flex")).toBe(true);
  });

  it("passes native attributes straight through", () => {
    const element = rendered(render(SplitView, { id: "inspector" }));
    expect(element.id).toBe("inspector");
  });
});
