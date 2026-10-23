import { afterEach, describe, expect, it } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import KpiCard from "../src/KpiCard.svelte";
import Table from "../src/Table.svelte";

const mounted: Array<{ instance: Record<string, unknown>; target: HTMLElement }> = [];

function render(component: Parameters<typeof mount>[0], props: Record<string, unknown>): HTMLElement {
  const target = document.createElement("div");
  document.body.append(target);
  const instance = mount(component, { target, props }) as Record<string, unknown>;
  mounted.push({ instance, target });
  flushSync();
  return target;
}

afterEach(() => {
  for (const entry of mounted.splice(0)) {
    unmount(entry.instance);
    entry.target.remove();
  }
});

describe("Data elevation", () => {
  it("separates metric cards from the canvas while keeping table headers opaque", () => {
    const kpi = render(KpiCard, { label: "Availability", value: "99.98%" })
      .querySelector<HTMLElement>("[data-kpi-card]")!;
    for (const token of ["bg-elevation-raised-surface", "border-muted"]) {
      expect(kpi.classList.contains(token), `KpiCard does not resolve ${token}`).toBe(true);
    }
    expect(kpi.classList.contains("bg-background")).toBe(false);
    expect(kpi.classList.contains("border-elevation-raised-border")).toBe(false);
    expect(kpi.classList.contains("shadow-elevation-raised")).toBe(false);

    const table = render(Table, {
      caption: "Deployments",
      columns: [{ key: "name", header: "Name" }],
      rows: [{ name: "api" }],
    });
    const header = table.querySelector<HTMLElement>("thead")!;
    for (const token of [
      "bg-background",
      "border-muted",
    ]) {
      expect(header.classList.contains(token), `Table header does not resolve ${token}`).toBe(true);
    }
    expect(header.classList.contains("sticky")).toBe(true);
  });
});
