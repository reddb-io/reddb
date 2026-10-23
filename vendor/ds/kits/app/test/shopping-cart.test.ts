import { flushSync } from "svelte";
import { createRawSnippet } from "svelte";
import { describe, expect, it, vi } from "vitest";
import ShoppingCart from "../src/composites/ShoppingCart.svelte";
import { shoppingCart } from "../src/composites/shopping-cart.variants";
import { expectCommerceReadiness } from "./commerce-readiness";
import CommerceContractFailures from "./fixtures/CommerceContractFailures.svelte";
import CommerceSurfacesConsumer from "./fixtures/CommerceSurfacesConsumer.svelte";
import { classes, classesOf, render, rendered } from "./mount";

function cartAnnouncementFailures(root: HTMLElement): string[] {
  const total = root.querySelector("output");
  return total?.getAttribute("aria-live") === "polite" &&
    total.getAttribute("aria-atomic") === "true"
    ? []
    : ["cart total is not announced when it changes"];
}

const ITEMS = [
  { id: "keyboard", name: "Mechanical keyboard", price: "$48.00", quantity: 1 },
] as const;

describe("the deliberately failing Shopping cart fixture", () => {
  it("diagnoses a total whose changes are never announced", () => {
    const broken = rendered(render(CommerceContractFailures, { failure: "cart-total-is-silent" }));
    expect(cartAnnouncementFailures(broken)).toEqual([
      "cart total is not announced when it changes",
    ]);
  });
});

describe("ShoppingCart", () => {
  it("composes a native Form with named quantities and a polite atomic total", () => {
    const form = rendered(
      render(ShoppingCart, {
        title: "Shopping cart",
        items: ITEMS,
        totalLabel: "Total",
        total: "$48.00",
      }),
    ) as HTMLFormElement;
    const quantity = form.querySelector<HTMLInputElement>('input[type="number"]')!;

    expect(form.tagName).toBe("FORM");
    expect(form.querySelector("li h3")?.textContent).toBe("Mechanical keyboard");
    expect(quantity.name).toBe("quantity[keyboard]");
    expect(new FormData(form).get(quantity.name)).toBe("1");
    expect(form.querySelector("output")?.textContent).toContain("$48.00");
    expect(cartAnnouncementFailures(form)).toEqual([]);
  });

  it.each([
    [1, "h2"],
    [3, "h4"],
    [6, "h6"],
  ] as const)("places item names one level below a level %i cart heading", (level, itemHeading) => {
    const form = rendered(
      render(ShoppingCart, {
        title: "Shopping cart",
        level,
        items: ITEMS,
        totalLabel: "Total",
        total: "$48.00",
      }),
    );

    expect(form.querySelector(`li ${itemHeading}`)?.textContent).toBe("Mechanical keyboard");
  });

  it("keeps quantity and removal controls in native focus order", () => {
    const onquantitychange = vi.fn();
    const onremove = vi.fn();
    const form = rendered(
      render(ShoppingCart, {
        title: "Shopping cart",
        items: ITEMS,
        totalLabel: "Total",
        total: "$48.00",
        onquantitychange,
        onremove,
        actions: createRawSnippet(() => ({ render: () => '<button type="submit">Checkout</button>' })),
      }),
    );
    const quantity = form.querySelector<HTMLInputElement>('input[type="number"]')!;
    const remove = form.querySelector<HTMLButtonElement>('button[type="button"]')!;

    quantity.focus();
    expect(document.activeElement).toBe(quantity);
    quantity.value = "2";
    quantity.dispatchEvent(new Event("input", { bubbles: true }));
    flushSync();
    expect(onquantitychange).toHaveBeenCalledWith("keyboard", 2);

    remove.focus();
    expect(document.activeElement).toBe(remove);
    remove.click();
    expect(onremove).toHaveBeenCalledWith("keyboard");
  });

  it("renders caller-owned item media, details, item actions, and an empty state", () => {
    const media = createRawSnippet(() => ({
      render: () => '<img src="/keyboard.webp" alt="Mechanical keyboard">',
    }));
    const details = createRawSnippet(() => ({
      render: () => '<p data-custom-details>Graphite · Type C</p>',
    }));
    const itemActions = createRawSnippet(() => ({
      render: () => '<a href="/save">Save for later</a>',
    }));
    const populated = rendered(
      render(ShoppingCart, {
        title: "Shopping cart",
        items: [{ ...ITEMS[0], media, details, actions: itemActions }],
        totalLabel: "Total",
        total: "$48.00",
      }),
    );

    expect(populated.querySelector("[data-cart-item-media] img")?.getAttribute("alt")).toBe(
      "Mechanical keyboard",
    );
    expect(populated.querySelector("[data-custom-details]")?.textContent).toContain(
      "Graphite · Type C",
    );
    expect(populated.querySelector('[data-cart-item-actions] a')?.getAttribute("href")).toBe(
      "/save",
    );

    const empty = rendered(
      render(ShoppingCart, {
        title: "Shopping cart",
        items: [],
        totalLabel: "Total",
        total: "$0.00",
        empty: createRawSnippet(() => ({
          render: () => '<p data-cart-recovery>Your basket is empty. Choose a product.</p>',
        })),
      }),
    );

    expect(empty.querySelector("[data-cart-empty] [data-cart-recovery]")).not.toBeNull();
    expect(empty.querySelector("[data-cart-total]")).toBeNull();
  });

  it("inherits nested appearance and keeps item rhythm Density-owned", () => {
    const scope = document.createElement("div");
    scope.dataset.theme = "application";
    scope.dataset.colorScheme = "dark";
    scope.dataset.density = "compact";
    const target = render(ShoppingCart, {
      title: "Shopping cart",
      items: ITEMS,
      totalLabel: "Total",
      total: "$48.00",
    });
    scope.append(...target.children);
    const cart = scope.querySelector<HTMLElement>("[data-shopping-cart]")!;
    const styles = shoppingCart();

    for (const name of classesOf(styles.root())) expect(classes(cart)).toContain(name);
    // The Form owns the rhythm between the cart's rows (ADR 0024).
    expect(classes(cart)).toContain("gap-[var(--reddb-spatial-layout-gap-md)]");
    expect(cart.hasAttribute("data-theme")).toBe(false);
    expect(cart.hasAttribute("data-color-scheme")).toBe(false);
    expect(cart.hasAttribute("data-density")).toBe(false);
  });

  it("consumer-compiles, exports, showcases, and declares complete readiness", () => {
    expect(render(CommerceSurfacesConsumer).querySelector("[data-shopping-cart]")).not.toBeNull();
    expectCommerceReadiness("shopping-cart", "ShoppingCart");
  });
});
