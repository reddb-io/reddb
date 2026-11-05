import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import ProductQuickview from "../src/composites/ProductQuickview.svelte";
import { productQuickview } from "../src/composites/product-quickview.variants";
import ProductCatalogueConsumer from "./fixtures/ProductCatalogueConsumer.svelte";
import ProductCatalogueContractFailures from "./fixtures/ProductCatalogueContractFailures.svelte";
import { labelInName } from "./label-in-name";
import { expectLayoutReadiness } from "./layout-readiness";
import { classes, classesOf, click, render, rendered } from "./mount";

describe("the deliberately failing product quickview fixture", () => {
  it("dismisses without restoring focus to the control that opened it", () => {
    const root = render(ProductCatalogueContractFailures);
    const trigger = root.querySelector<HTMLButtonElement>("[data-broken-quickview-trigger]")!;
    const dialog = root.querySelector<HTMLDialogElement>("[data-broken-quickview]")!;
    const action = root.querySelector<HTMLButtonElement>("[data-broken-quickview-action]")!;

    trigger.focus();
    click(trigger);
    expect(document.activeElement).toBe(action);
    dialog.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));

    expect(dialog.open).toBe(false);
    expect(document.activeElement).not.toBe(trigger);
  });
});

describe("ProductQuickview", () => {
  it("composes the canonical Dialog labeling and focus-return lifecycle", () => {
    const children = createRawSnippet(() => ({
      render: () => '<button type="button" data-buy>Add to basket</button>',
    }));
    const root = render(ProductQuickview, {
      triggerLabel: "Quick view Field notes",
      title: "Field notes",
      description: "A pocket notebook.",
      children,
    });
    const wrapper = rendered(root);
    const trigger = root.querySelector<HTMLButtonElement>("[data-dialog-trigger]")!;
    const dialog = root.querySelector<HTMLDialogElement>("dialog")!;

    expect(wrapper.matches("[data-product-quickview]")).toBe(true);
    expect(trigger.getAttribute("aria-label")).toBe("Quick view Field notes");
    trigger.focus();
    click(trigger);
    expect(document.activeElement).toBe(root.querySelector("[data-buy]"));
    dialog.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(dialog.open).toBe(false);
    expect(document.activeElement).toBe(trigger);
  });

  it("names a custom trigger with a label that starts with the words it shows (WCAG 2.5.3)", () => {
    const shown = (text: string) => createRawSnippet(() => ({ render: () => `<span>${text}</span>` }));
    const named = render(ProductQuickview, {
      triggerLabel: "Quick view Field notes",
      title: "Field notes",
      trigger: shown("Quick view"),
    }).querySelector<HTMLButtonElement>("[data-dialog-trigger]")!;
    const plain = render(ProductQuickview, {
      triggerLabel: "Quick view Field notes",
      title: "Field notes",
    }).querySelector<HTMLButtonElement>("[data-dialog-trigger]")!;
    const mismatched = render(ProductQuickview, {
      triggerLabel: "Quick view Field notes",
      title: "Field notes",
      trigger: shown("Preview Field notes"),
    }).querySelector<HTMLButtonElement>("[data-dialog-trigger]")!;

    expect(named.getAttribute("aria-label")).toBe("Quick view Field notes");
    expect(labelInName(named)).toBe(true);
    expect(plain.textContent?.trim()).toBe("Quick view Field notes");
    expect(labelInName(plain)).toBe(true);
    // The shape wave 6B removed from the showcase: the visible words are not in the name.
    expect(labelInName(mismatched)).toBe(false);
  });

  it("contains Tab navigation within the quickview", () => {
    const children = createRawSnippet(() => ({
      render: () =>
        '<span><button type="button" data-first-action>Choose size</button><button type="button" data-last-action>Add</button></span>',
    }));
    const root = render(ProductQuickview, {
      triggerLabel: "Quick view Field notes",
      title: "Field notes",
      children,
    });
    click(root.querySelector<HTMLButtonElement>("[data-dialog-trigger]")!);
    const dialog = root.querySelector<HTMLDialogElement>("dialog")!;
    const close = root.querySelector<HTMLButtonElement>("[data-dialog-close]")!;
    const last = root.querySelector<HTMLButtonElement>("[data-last-action]")!;

    last.focus();
    dialog.dispatchEvent(new KeyboardEvent("keydown", { key: "Tab", bubbles: true }));
    expect(document.activeElement).toBe(close);
  });

  it("routes quickview rhythm through Density and inherits nested appearance", () => {
    const root = render(ProductCatalogueConsumer);
    const quickview = root.querySelector<HTMLElement>("[data-product-quickview]")!;

    expect(classes(quickview)).toEqual(classesOf(productQuickview().root()));
    expect(classes(quickview.querySelector("[data-product-quickview-content]")!).has(
      "gap-[var(--reddb-spatial-layout-gap-md)]",
    )).toBe(true);
    expect(quickview.closest('[data-theme="base"][data-color-scheme="dark"][data-density="compact"]'))
      .not.toBeNull();
    expect(quickview.hasAttribute("data-theme")).toBe(false);
    expect(quickview.hasAttribute("data-color-scheme")).toBe(false);
    expect(quickview.hasAttribute("data-density")).toBe(false);
  });

  it("ships showcase, consumer, distributed export, and readiness evidence", () => {
    expectLayoutReadiness("product-quickview", "ProductQuickview");
  });
});
