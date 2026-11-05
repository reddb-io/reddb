// Pill, as an application meets it.
//
// The dismiss affordance carries the only real behavior in this Primitive, so
// most of what is asserted here is about it: that it appears only when there
// is something for it to do, that it is nameable, and that pressing it calls
// back exactly once.

import { describe, expect, it, vi } from "vitest";
import Pill from "../src/primitives/Pill.svelte";
import { PILL_SIZES, PILL_VARIANTS, pill } from "../src/primitives/pill.variants";
import { classes, classesOf, click, render, rendered, text } from "./mount";

describe("Pill", () => {
  it("renders a <span> carrying its children", () => {
    const element = rendered(render(Pill, { children: text("svelte") }));
    expect(element.tagName).toBe("SPAN");
    expect(element.textContent?.trim()).toBe("svelte");
  });

  it("wears exactly the classes its variants module produces", () => {
    for (const variant of PILL_VARIANTS) {
      for (const size of PILL_SIZES) {
        const element = rendered(render(Pill, { variant, size }));
        expect(classes(element)).toEqual(classesOf(pill({ variant, size })));
      }
    }
  });

  it("defaults to the neutral tone, tinted, at the medium size", () => {
    const element = rendered(render(Pill, {}));
    expect(classes(element)).toEqual(classesOf(pill({ tone: "neutral", variant: "tinted", size: "md" })));
    expect(element.dataset.tone).toBe("neutral");
    expect(element.dataset.variant).toBe("tinted");
  });

  it("renders no dismiss affordance when there is nothing to dismiss to", () => {
    expect(rendered(render(Pill, {})).querySelector("button")).toBeNull();
  });

  it("dismisses once per press, through a named control", () => {
    const ondismiss = vi.fn();
    const element = rendered(render(Pill, { ondismiss, dismissLabel: "Remove svelte" }));
    const dismiss = element.querySelector("button");
    expect(dismiss).not.toBeNull();
    expect(dismiss!.getAttribute("aria-label")).toBe("Remove svelte");
    // A dismiss inside a form must not submit it on its way out.
    expect(dismiss!.getAttribute("type")).toBe("button");

    click(dismiss as HTMLElement);
    expect(ondismiss).toHaveBeenCalledTimes(1);
  });

  it("names the dismiss affordance even when the caller forgets to", () => {
    const element = rendered(render(Pill, { ondismiss: () => {} }));
    expect(element.querySelector("button")!.getAttribute("aria-label")).toBe("Remove");
  });

  it("merges a caller's classes over its own", () => {
    const element = rendered(render(Pill, { class: "max-w-40" }));
    expect(classes(element).has("max-w-40")).toBe(true);
    expect(classes(element).has("rounded-full")).toBe(true);
  });
});
