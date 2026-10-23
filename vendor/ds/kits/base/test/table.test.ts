import { describe, expect, it } from "vitest";
import { Table, table as tableAppearance } from "@reddb-io/design-system/base";
import TableContractFailures from "./fixtures/TableContractFailures.svelte";
import TableConsumer from "./fixtures/TableConsumer.svelte";
import { classes, render, rendered } from "./mount";

describe("the deliberately failing Table fixtures", () => {
  it("demonstrates tabular data without native header association", () => {
    const root = render(TableContractFailures, { failure: "missing-header-association" });
    const table = root.querySelector<HTMLTableElement>("[data-broken-table]")!;

    expect(table.querySelector("caption")).toBeNull();
    expect(table.querySelectorAll("th")).toHaveLength(0);
    expect(table.querySelectorAll("[scope]")).toHaveLength(0);
  });

  it("demonstrates a wide table whose overflow is silent and unreachable", () => {
    const root = render(TableContractFailures, { failure: "silent-overflow" });
    const region = root.querySelector<HTMLElement>("[data-broken-table-overflow]")!;

    expect(classes(region).has("overflow-hidden")).toBe(true);
    expect(classes(region).has("overflow-x-auto")).toBe(false);
    expect(region.tabIndex).toBe(-1);
  });
});

describe("the Base Table", () => {
  it("owns a visible caption and associates column and row headers", () => {
    const root = render(Table, {
      caption: "Cluster replicas",
      columns: [
        { key: "node", header: "Node" },
        { key: "region", header: "Region" },
        { key: "latency", header: "Latency" },
      ],
      rows: [
        { node: "db-01", region: "South America", latency: "24 ms" },
        { node: "db-02", region: "Europe", latency: "31 ms" },
      ],
      rowHeader: "node",
    });
    const table = root.querySelector<HTMLTableElement>("table")!;

    expect(table.caption?.textContent).toBe("Cluster replicas");
    expect([...table.querySelectorAll("thead th")].map((cell) => cell.scope)).toEqual([
      "col",
      "col",
      "col",
    ]);
    expect([...table.querySelectorAll("tbody th")].map((cell) => cell.scope)).toEqual([
      "row",
      "row",
    ]);
    expect([...table.rows[1]!.cells].map((cell) => cell.textContent)).toEqual([
      "db-01",
      "South America",
      "24 ms",
    ]);
  });

  it("makes responsive overflow named, visible, and keyboard-scrollable", () => {
    const region = rendered(
      render(Table, {
        caption: "Cluster replicas",
        columns: [
          { key: "node", header: "Node" },
          { key: "region", header: "Region" },
        ],
        rows: [{ node: "db-01", region: "South America" }],
      }),
    );
    const caption = region.querySelector("caption")!;

    expect(region.getAttribute("role")).toBe("region");
    expect(region.getAttribute("aria-labelledby")).toBe(caption.id);
    expect(classes(region).has("overflow-x-auto")).toBe(true);
    expect(region.tabIndex).toBe(0);
    region.focus();
    expect(document.activeElement).toBe(region);
  });

  it("keeps token appearance and every appearance axis live in a nested scope", () => {
    const root = render(TableConsumer);
    const scope = root.querySelector<HTMLElement>("[data-table-scope]")!;
    const region = scope.querySelector<HTMLElement>("[data-table-scroll]")!;
    const element = region.querySelector<HTMLTableElement>("[data-table]")!;
    const styles = tableAppearance();

    expect(classes(region)).toEqual(new Set(styles.root().split(/\s+/)));
    expect(classes(element)).toEqual(new Set(styles.table().split(/\s+/)));
    expect(
      classes(element.querySelector("tbody td")!).has("px-[var(--reddb-spatial-inset-md)]"),
    ).toBe(true);
    expect(scope.getAttribute("data-density")).toBe("compact");
    expect(region.hasAttribute("data-theme")).toBe(false);
    expect(region.hasAttribute("data-color-scheme")).toBe(false);
    expect(region.hasAttribute("data-density")).toBe(false);
  });

  it("renders caller-owned cell content with its native focus behavior intact", () => {
    const root = render(TableConsumer);
    const link = root.querySelector<HTMLAnchorElement>('tbody th a[href="/nodes/db-01"]')!;

    expect(link.textContent).toBe("db-01");
    link.focus();
    expect(document.activeElement).toBe(link);
  });
});
