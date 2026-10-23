import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import { Container, container as containerAppearance } from "./fixtures/layout-consumer";
import LayoutConsumer from "./fixtures/LayoutConsumer.svelte";
import { classes, classesOf, render, rendered } from "./mount";

describe("the Base Container", () => {
  it("bounds content with a Density-owned inline inset", () => {
    const root = render(Container, {
      "aria-label": "Account content",
      children: createRawSnippet(() => ({ render: () => "<p>Account</p>" })),
    });
    const element = rendered(root);

    expect(element.tagName).toBe("DIV");
    expect(element.getAttribute("aria-label")).toBe("Account content");
    expect(classes(element)).toEqual(classesOf(containerAppearance()));
    // `wide` (80rem, the former max-w-7xl) is the default DS-named width (ADR 0021).
    expect(classes(element).has("max-w-wide")).toBe(true);
    expect(element.dataset.containerSize).toBe("wide");
    expect(classes(element).has("px-[var(--reddb-spatial-inset-md)]")).toBe(true);
    expect(element.textContent).toBe("Account");
  });

  it("merges a local class without selecting Density", () => {
    const element = rendered(render(Container, { class: "relative" }));

    expect(classes(element)).toEqual(classesOf(containerAppearance({ class: "relative" })));
    expect(element.hasAttribute("data-density")).toBe(false);
  });

  it("composes inside a nested Density scope", () => {
    const root = render(LayoutConsumer);
    const outer = root.querySelector<HTMLElement>('[aria-label="Deployment content"]')!;
    const inner = root.querySelector<HTMLElement>('[aria-label="Compact nested content"]')!;

    expect(classes(outer)).toEqual(classesOf(containerAppearance()));
    expect(classes(inner)).toEqual(classesOf(containerAppearance()));
    expect(inner.closest('[data-density="compact"]')).not.toBeNull();
  });
});
