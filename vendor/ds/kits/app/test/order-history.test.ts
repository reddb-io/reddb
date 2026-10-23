import { describe, expect, it } from "vitest";
import OrderHistory from "../src/composites/OrderHistory.svelte";
import { orderHistory } from "../src/composites/order-history.variants";
import { expectCommerceReadiness } from "./commerce-readiness";
import CommerceSurfacesConsumer from "./fixtures/CommerceSurfacesConsumer.svelte";
import { classes, classesOf, render, rendered } from "./mount";

const ORDERS = [
  {
    id: "RDB-1042",
    placed: "8 August 2026",
    placedAt: "2026-08-08",
    status: "Delivered",
    total: "$53.00",
  },
] as const;

describe("OrderHistory", () => {
  it("composes a named native table with scoped headers and machine-readable dates", () => {
    const history = rendered(
      render(OrderHistory, {
        title: "Order history",
        caption: "Past orders",
        orders: ORDERS,
      }),
    );
    const table = history.querySelector("table")!;

    expect(history.tagName).toBe("SECTION");
    expect(history.getAttribute("aria-labelledby")).toBe(
      history.querySelector("[data-section-heading]")?.id,
    );
    expect(table.querySelector("caption")?.textContent).toBe("Past orders");
    expect([...table.querySelectorAll("thead th")].map((cell) => cell.getAttribute("scope")))
      .toEqual(["col", "col", "col", "col"]);
    expect(table.querySelector("tbody th")?.getAttribute("scope")).toBe("row");
    expect(table.querySelector("time")?.getAttribute("datetime")).toBe("2026-08-08");
    expect(table.textContent).toContain("Delivered");
  });

  it("keeps an overflowing history keyboard reachable", () => {
    const history = rendered(
      render(OrderHistory, { title: "Order history", caption: "Past orders", orders: ORDERS }),
    );
    const scroll = history.querySelector<HTMLElement>("[data-table-scroll]")!;

    expect(scroll.tabIndex).toBe(0);
    scroll.focus();
    expect(document.activeElement).toBe(scroll);
  });

  it("inherits nested appearance and keeps history spacing Density-owned", () => {
    const scope = document.createElement("div");
    scope.dataset.theme = "application";
    scope.dataset.colorScheme = "dark";
    scope.dataset.density = "compact";
    const target = render(OrderHistory, {
      title: "Order history",
      caption: "Past orders",
      orders: ORDERS,
    });
    scope.append(...target.children);
    const history = scope.querySelector<HTMLElement>("[data-order-history]")!;
    const styles = orderHistory();

    for (const name of classesOf(styles.root())) expect(classes(history)).toContain(name);
    // A group's heading sits a layout step above its content (ADR 0024).
    expect(classes(history)).toContain("gap-[var(--reddb-spatial-layout-gap-sm)]");
    expect(history.hasAttribute("data-theme")).toBe(false);
    expect(history.hasAttribute("data-color-scheme")).toBe(false);
    expect(history.hasAttribute("data-density")).toBe(false);
  });

  it("consumer-compiles, exports, showcases, and declares complete readiness", () => {
    expect(render(CommerceSurfacesConsumer).querySelector("[data-order-history]")).not.toBeNull();
    expectCommerceReadiness("order-history", "OrderHistory");
  });
});
