import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import OrderSummary from "../src/composites/OrderSummary.svelte";
import { orderSummary } from "../src/composites/order-summary.variants";
import { expectCommerceReadiness } from "./commerce-readiness";
import CommerceSurfacesConsumer from "./fixtures/CommerceSurfacesConsumer.svelte";
import { classes, classesOf, render, rendered } from "./mount";

const LINES = [
  { label: "Subtotal", value: "$48.00" },
  { label: "Delivery", value: "$5.00" },
] as const;

describe("OrderSummary", () => {
  it("composes a visibly named region from canonical Card and DescriptionList", () => {
    const summary = rendered(
      render(OrderSummary, {
        title: "Order summary",
        lines: LINES,
        totalLabel: "Total",
        total: "$53.00",
      }),
    );

    expect(summary.getAttribute("role")).toBe("region");
    expect(summary.getAttribute("aria-labelledby")).toBe(
      summary.querySelector("[data-section-heading]")?.id,
    );
    expect(summary.querySelectorAll("[data-description-item]")).toHaveLength(2);
    expect(summary.querySelector("[data-order-summary-total] output")?.textContent).toBe("$53.00");
  });

  it("keeps caller-owned actions in native keyboard focus", () => {
    const summary = rendered(
      render(OrderSummary, {
        title: "Order summary",
        lines: LINES,
        totalLabel: "Total",
        total: "$53.00",
        actions: createRawSnippet(() => ({ render: () => '<button type="button">Apply credit</button>' })),
      }),
    );
    const action = summary.querySelector<HTMLButtonElement>("button")!;

    action.focus();
    expect(document.activeElement).toBe(action);
  });

  it("inherits nested appearance and keeps summary spacing Density-owned", () => {
    const scope = document.createElement("div");
    scope.dataset.theme = "application";
    scope.dataset.colorScheme = "dark";
    scope.dataset.density = "compact";
    const target = render(OrderSummary, {
      title: "Order summary",
      lines: LINES,
      totalLabel: "Total",
      total: "$53.00",
    });
    scope.append(...target.children);
    const summary = scope.querySelector<HTMLElement>("[data-order-summary]")!;
    const styles = orderSummary();

    for (const name of classesOf(styles.root())) expect(classes(summary)).toContain(name);
    expect(classes(summary.querySelector("[data-order-summary-total]")!)).toContain(
      "gap-[var(--reddb-spatial-gap-md)]",
    );
    expect(summary.hasAttribute("data-theme")).toBe(false);
    expect(summary.hasAttribute("data-color-scheme")).toBe(false);
    expect(summary.hasAttribute("data-density")).toBe(false);
  });

  it("consumer-compiles, exports, showcases, and declares complete readiness", () => {
    expect(render(CommerceSurfacesConsumer).querySelector("[data-order-summary]")).not.toBeNull();
    expectCommerceReadiness("order-summary", "OrderSummary");
  });
});
