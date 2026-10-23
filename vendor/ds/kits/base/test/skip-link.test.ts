import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import { SkipLink, skipLink as skipLinkAppearance } from "./fixtures/layout-consumer";
import LayoutConsumer from "./fixtures/LayoutConsumer.svelte";
import { classes, classesOf, render, rendered } from "./mount";

describe("the Base SkipLink", () => {
  it("targets the canonical main landmark with a useful default label", () => {
    const root = render(LayoutConsumer);
    const link = root.querySelector<HTMLAnchorElement>("[data-skip-link]")!;
    const target = root.querySelector<HTMLElement>(link.hash)!;

    expect(link.getAttribute("href")).toBe("#main-content");
    expect(link.textContent).toBe("Skip to main content");
    expect(target.tagName).toBe("MAIN");
    expect(target.tabIndex).toBe(-1);

    link.click();
    expect(document.activeElement).toBe(target);
  });

  it("is keyboard focusable and carries a focus-visible escape from visual hiding", () => {
    const link = rendered(render(SkipLink)) as HTMLAnchorElement;

    link.focus();

    expect(document.activeElement).toBe(link);
    expect(classes(link)).toEqual(classesOf(skipLinkAppearance()));
    expect(classes(link).has("sr-only")).toBe(true);
    expect(classes(link).has("focus:not-sr-only")).toBe(true);
    expect(classes(link).has("focus-visible:outline-focus")).toBe(true);
  });

  it("preserves native anchor attributes and caller-owned content", () => {
    const link = rendered(
      render(SkipLink, {
        href: "#article",
        rel: "bookmark",
        class: "uppercase",
        children: createRawSnippet(() => ({ render: () => "<span>Jump to article</span>" })),
      }),
    ) as HTMLAnchorElement;

    expect(link.getAttribute("href")).toBe("#article");
    expect(link.getAttribute("rel")).toBe("bookmark");
    expect(link.textContent).toBe("Jump to article");
    expect(classes(link)).toEqual(classesOf(skipLinkAppearance({ class: "uppercase" })));
  });
});
