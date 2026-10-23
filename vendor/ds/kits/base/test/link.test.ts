import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import { Link, LinkPreview, link as linkAppearance } from "@reddb-io/design-system/base";
import InlineTextContractFailures from "./fixtures/InlineTextContractFailures.svelte";
import { classes, classesOf, render, rendered } from "./mount";

const text = (value: string) =>
  createRawSnippet(() => ({ render: () => `<span>${value}</span>` }));

describe("the deliberately failing Link fixture", () => {
  it("demonstrates a link distinguishable from prose by colour alone", () => {
    const link = rendered(
      render(InlineTextContractFailures, { failure: "colour-only-link" }),
    ).querySelector<HTMLAnchorElement>("[data-broken-link]")!;

    expect(classes(link).has("text-primary")).toBe(true);
    expect([...classes(link)].some((name) => name.includes("underline"))).toBe(false);
  });
});

describe("the Base Link", () => {
  it("keeps native link semantics and a non-colour affordance", () => {
    const link = rendered(
      render(Link, {
        href: "/guide",
        children: text("Usage guide"),
        class: "font-medium",
      }),
    ) as HTMLAnchorElement;

    expect(link.tagName).toBe("A");
    expect(link.getAttribute("href")).toBe("/guide");
    expect(link.textContent).toBe("Usage guide");
    expect(classes(link)).toEqual(classesOf(linkAppearance({ class: "font-medium" })));
    expect(classes(link).has("underline")).toBe(true);

    link.focus();
    expect(document.activeElement).toBe(link);
    expect(classes(link).has("focus-visible:outline-focus")).toBe(true);
  });

  it("is the canonical native-link affordance composed by LinkPreview", () => {
    const root = render(LinkPreview, {
      href: "/guide",
      label: "Usage guide",
      previewLabel: "Usage guide preview",
      triggerClass: "font-medium",
    });
    const trigger = root.querySelector<HTMLAnchorElement>("[data-link-preview-trigger]")!;

    expect(classes(trigger)).toEqual(classesOf(linkAppearance({ class: "font-medium" })));
  });

  it("is ink, not accent: foreground text with the underline, thickening on hover", () => {
    const link = rendered(render(Link, { href: "/guide", children: text("Usage guide") }));
    const names = classes(link);

    expect(names.has("text-foreground")).toBe(true);
    expect(names.has("underline")).toBe(true);
    expect(names.has("hover:decoration-2")).toBe(true);
    expect([...names].some((name) => /^(?:hover:)?text-primary/.test(name))).toBe(false);
  });
});
