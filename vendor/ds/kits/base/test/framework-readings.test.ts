import { mount, unmount, flushSync } from "svelte";
import { act, createElement as h, type ComponentType } from "react";
import { createRoot } from "react-dom/client";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import * as react from "../react/src";
import Fixture from "./fixtures/FrameworkReadings.svelte";
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
type Scenario = { name: "Table" | "Timeline" | "Steps" | "Pagination" | "ScrollArea" | "Countdown" | "Breadcrumbs"; options?: Record<string, unknown>; rich?: boolean };
const columns = [{ key: "node", header: "Node" }, { key: "latency", header: "Latency" }, { key: "missing", header: "Details" }];
const rows = [{ node: "db-01", latency: 0 }, { node: "db-02", latency: 24, missing: null }];
const timeline = [{ id: "a", title: "Created", datetime: "2026-10-19T10:00:00Z", time: "10:00", description: "New workspace" }, { id: "b", title: "Ready", datetime: "2026-10-19T10:01:00Z", time: "10:01", href: "/ready" }];
function props({ name, options = {}, rich }: Scenario) {
  const native = ["Steps", "Pagination", "Breadcrumbs"].includes(name) ? {} : { "data-consumer": "forwarded" };
  const common = { ...options, className: "consumer-extra", ...native };
  if (name === "Table" && rich) return { ...common, cell: ({ column, rowIndex, value }: react.TableCellContext) => h("a", { href: `/cell/${rowIndex}/${column.key}` }, value ?? "Missing") };
  if (name === "ScrollArea") return { ...common, children: [h("a", { key: "a", href: "/detail" }, "Details"), h("p", { key: "p" }, "Scrollable content")] };
  return common;
}
async function render(scenario: Scenario) {
  const a = document.createElement("div"); const b = document.createElement("div"); document.body.append(a, b);
  const native = (options: Record<string, unknown> = {}) => ({ ...options, class: "consumer-extra", ...(["Steps", "Pagination", "Breadcrumbs"].includes(scenario.name) ? {} : { "data-consumer": "forwarded" }) });
  const svelte = mount(Fixture, { target: a, props: { ...scenario, options: native(scenario.options) } }); flushSync();
  const root = createRoot(b);
  await act(() => root.render(h(react[scenario.name] as ComponentType<any>, props(scenario))));
  return { a, b, update: async (options: Record<string, unknown>) => {
    flushSync(() => svelte.update(native(options)));
    await act(() => root.render(h(react[scenario.name] as ComponentType<any>, props({ ...scenario, options }))));
  }, cleanup: async () => { await act(() => root.unmount()); await unmount(svelte); a.remove(); b.remove(); } };
}
function snapshot(target: HTMLElement) {
  const ids = [...target.querySelectorAll("[id]")].map(element => element.id);
  const normalize = (value: string) => value.split(" ").map(token => ids.includes(token) ? `id-${ids.indexOf(token)}` : token).join(" ");
  return [...target.querySelectorAll("*")].map(element => ({ tag: element.tagName, classes: [...element.classList].sort(),
    attributes: Object.fromEntries([...element.attributes].filter(a => a.name !== "class" && a.name !== "style" && !a.name.startsWith("data-svelte"))
      .map(a => [a.name, ["id", "aria-labelledby"].includes(a.name) ? normalize(a.value) : a.name.startsWith("data-") && a.value === "true" ? "" : a.value]).sort()),
    style: element.getAttribute("style") ? Object.fromEntries([...((element as HTMLElement).style)].sort().map(n => [n, (element as HTMLElement).style.getPropertyValue(n)])) : undefined,
    text: element.children.length ? undefined : element.textContent?.trim() }));
}
const scenarios: Scenario[] = [
  ...[undefined, "latency", "absent"].flatMap(rowHeader => [false, true].map(rich => ({ name: "Table" as const, rich, options: { caption: "Cluster replicas", columns, rows, ...(rowHeader === undefined ? {} : { rowHeader }), containerClass: "region-extra", captionClass: "caption-extra", headerClass: "header-extra", rowClass: "row-extra", headClass: "head-extra", cellClass: "cell-extra" } }))),
  { name: "Table", options: { caption: "Empty", columns: [], rows: [] } },
  { name: "Table", options: { caption: "No rows", columns, rows: [] } },
  { name: "Timeline", options: { items: timeline } }, { name: "Timeline", options: { items: [], label: "Changes" } },
  ...["complete", "current", "upcoming"].flatMap(state => [undefined, "/step"].map(href => ({ name: "Steps" as const, options: { items: [{ id: "one", label: "Create", state, href }], label: "Provisioning" } }))),
  { name: "Steps", options: { items: [] } },
  { name: "Pagination", options: { pages: [{ page: 1, href: "/?page=1" }, { page: 2, href: "/?page=2", current: true }] } },
  { name: "Pagination", options: { pages: [], label: "Result pages" } },
  ...["vertical", "horizontal", "both"].flatMap(orientation => ["outline", "plain"].map(variant => ({ name: "ScrollArea" as const, options: { orientation, variant, label: "Activity" } }))),
  ...[0, -1, Number.NaN, Infinity, 0.99, 1, 65.9, 3725].map(remaining => ({ name: "Countdown" as const, options: { remaining } })),
  { name: "Countdown", options: { remaining: 65.9, label: "Retry", completeLabel: "Ready", formatValue: (value: number) => `${value} seconds` } },
  { name: "Breadcrumbs", options: { items: [{ id: "home", label: "Workspace", href: "/" }, { id: "current", label: "Production", current: true }] } },
];
for (const scenario of scenarios) it(`matches ${scenario.name} native content, appearance and semantics ${JSON.stringify(scenario.options ?? {})}`, async () => {
  const r = await render(scenario);
  try { expect(snapshot(r.b)).toEqual(snapshot(r.a)); } finally { await r.cleanup(); }
});
for (const framework of ["svelte", "react"] as const) describe(`${framework} readings behavior`, () => {
  it("keeps Table's scroll caption id stable while rows and row headers change", async () => {
    const r = await render({ name: "Table", rich: true, options: { caption: "Replicas", columns, rows } });
    try {
      const target = framework === "react" ? r.b : r.a;
      const region = target.querySelector<HTMLElement>("[data-table-scroll]")!; const id = region.getAttribute("aria-labelledby");
      region.focus(); expect(document.activeElement).toBe(region);
      expect(target.querySelector('tbody th[scope="row"] a')?.textContent).toBe("db-01");
      await r.update({ caption: "Updated replicas", columns, rows: rows.slice(1), rowHeader: "latency" });
      expect(region.getAttribute("aria-labelledby")).toBe(id);
      expect(target.querySelector("caption")?.id).toBe(id);
      expect(target.querySelector("caption")?.textContent).toBe("Updated replicas");
      expect(target.querySelector('tbody th[scope="row"] a')?.textContent).toBe("24");
      expect(target.querySelectorAll("tbody tr")).toHaveLength(1);
      const link = target.querySelector<HTMLAnchorElement>("tbody a")!; link.focus(); expect(document.activeElement).toBe(link);
    } finally { await r.cleanup(); }
  });
  it("updates countdown urgency only at completion without owning a timer", async () => {
    const r = await render({ name: "Countdown", options: { remaining: 65 } });
    try {
      const target = framework === "react" ? r.b : r.a;
      expect(target.querySelector('[role="timer"]')?.getAttribute("aria-live")).toBe("off");
      expect(target.querySelector("time")?.getAttribute("datetime")).toBe("PT1M5S");
      await r.update({ remaining: 0, completeLabel: "Ready" });
      expect(target.querySelector('[role="status"]')?.getAttribute("aria-live")).toBe("polite");
      expect(target.querySelector("time")?.textContent).toBe("Ready");
    } finally { await r.cleanup(); }
  });
  it("updates current step/page semantics without inventing selection state", async () => {
    const steps = await render({ name: "Steps", options: { items: [{ id: "a", label: "Create", state: "current" }, { id: "b", label: "Deploy", state: "upcoming", href: "/deploy" }] } });
    try {
      const target = framework === "react" ? steps.b : steps.a;
      expect(target.querySelector('[aria-current="step"]')?.textContent).toContain("Create");
      await steps.update({ items: [{ id: "a", label: "Create", state: "complete" }, { id: "b", label: "Deploy", state: "current", href: "/deploy" }] });
      expect(target.querySelector('[aria-current="step"]')?.textContent).toContain("Deploy");
      expect(target.querySelector('a[href="/deploy"]')).toBeNull();
      expect(target.textContent).toContain("Completed");
    } finally { await steps.cleanup(); }
    const pages = await render({ name: "Pagination", options: { pages: [{ page: 1, href: "/1", current: true }, { page: 2, href: "/2" }] } });
    try {
      const target = framework === "react" ? pages.b : pages.a;
      await pages.update({ pages: [{ page: 1, href: "/1" }, { page: 2, href: "/2", current: true }] });
      expect(target.querySelector('[aria-current="page"]')?.getAttribute("href")).toBe("/2");
      expect(target.querySelectorAll('[aria-current="page"]')).toHaveLength(1);
    } finally { await pages.cleanup(); }
  });
});
it("renders native Table caption and custom cells in React SSR", () => {
  const html = renderToString(h(react.Table, { caption: "Replicas", columns, rows, cell: ({ value }) => h("strong", null, value) }));
  const target = document.createElement("div"); target.innerHTML = html;
  expect(target.querySelector('[role="region"]')?.getAttribute("aria-labelledby")).toBe(target.querySelector("caption")?.id);
  expect(target.querySelector('tbody th[scope="row"] strong')?.textContent).toBe("db-01");
});
