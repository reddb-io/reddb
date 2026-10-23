// One event dialect (ADR 0026), as the Data Kit speaks it.
//
// The Data Kit was controlled-only with camelCase callbacks (`onSort`,
// `onSelected`, `onChanges`). It now reports a state `<state>` through
// `on<state>change(value)`, a value control through `onvaluechange`, a
// discrete event through a lowercase `on<noun>` (`onrename`, `onreorder`,
// `oninteraction`), and item activation — a row, a document — through the
// sanctioned `onselect`. Each camelCase name stays one release as an alias
// that still fires and warns once in development, and the Kit itself never
// passes a deprecated name to a parent component, so no warning fires from
// inside it.

import { flushSync, mount, tick, unmount } from "svelte";
import { afterEach, beforeEach, describe, expect, it, vi, type MockInstance } from "vitest";
import CodeField from "../src/CodeField.svelte";
import DocumentViewer from "../src/DocumentViewer.svelte";
import Graph from "../src/Graph.svelte";
import JsonTree from "../src/JsonTree.svelte";
import KpiCard from "../src/KpiCard.svelte";
import Table from "../src/Table.svelte";
import TreeView from "../src/TreeView.svelte";
import XmlTree from "../src/XmlTree.svelte";
import type { TableColumn } from "../src/table";

const mounted: Array<{ instance: Record<string, unknown>; target: HTMLElement }> = [];

function render(component: Parameters<typeof mount>[0], props: Record<string, unknown>): HTMLElement {
  const target = document.createElement("div");
  document.body.append(target);
  const instance = mount(component, { target, props }) as Record<string, unknown>;
  mounted.push({ instance, target });
  flushSync();
  return target;
}

function press(element: Element, key: string): void {
  element.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }));
  flushSync();
}

function type(element: HTMLInputElement, value: string): void {
  element.value = value;
  element.dispatchEvent(new Event("input", { bubbles: true }));
  flushSync();
}

let warn: MockInstance<typeof console.warn>;
beforeEach(() => {
  warn = vi.spyOn(console, "warn").mockImplementation(() => {});
});
afterEach(() => {
  warn.mockRestore();
  for (const entry of mounted.splice(0)) {
    unmount(entry.instance);
    entry.target.remove();
  }
});

const warnings = () => warn.mock.calls.map(([message]) => String(message));

const columns: readonly TableColumn[] = [
  { key: "region", header: "Region", sortable: true, filterable: true },
  { key: "replicas", header: "Replicas", editable: true },
];
const rows = [
  { id: "a", region: "São Paulo", replicas: 3 },
  { id: "b", region: "Frankfurt", replicas: 2 },
];
const nodes = [
  {
    id: "projects",
    label: "Projects",
    children: [
      { id: "projects/index", label: "index.ts", children: [] },
      { id: "projects/utils", label: "utils.ts", children: [] },
    ],
  },
];
const documents = [
  { id: "one", title: "First", body: "One." },
  { id: "two", title: "Second", body: "Two." },
];
const graph = {
  nodes: [{ id: "gateway" }, { id: "api" }],
  edges: [{ id: "route", source: "gateway", target: "api" }],
};

/** Drive one full Table session: sort, filter, page, edit and activate a row. */
async function driveTable(root: HTMLElement): Promise<void> {
  root.querySelector<HTMLButtonElement>("thead button")!.click();
  flushSync();
  root.querySelector<HTMLButtonElement>('nav button:last-of-type')!.click();
  flushSync();
  const cells = () => [...root.querySelectorAll<HTMLElement>("tbody [data-table-cell]")];
  press(cells()[1]!, "Enter");
  await tick();
  const editor = root.querySelector<HTMLInputElement>("input[data-table-editor]")!;
  type(editor, "9");
  press(editor, "Enter");
  cells()[0]!.click();
  flushSync();
  type(root.querySelector<HTMLInputElement>('input[aria-label="Filter Region"]')!, "fra");
}

describe("the Data Kit's canonical callbacks", () => {
  it("Table reports its sort, filters, page and staged changes as on<state>change, and a row through onselect", async () => {
    const calls = {
      onsortchange: vi.fn(),
      onfilterschange: vi.fn(),
      onpagechange: vi.fn(),
      onchangeschange: vi.fn(),
      onselect: vi.fn(),
    };
    const root = render(Table, { caption: "Regions", columns, rows, rowKey: "id", editable: true, pageSize: 1, ...calls });
    await driveTable(root);

    expect(calls.onsortchange).toHaveBeenLastCalledWith({ key: "region", direction: "ascending" });
    expect(calls.onpagechange).toHaveBeenLastCalledWith(2);
    expect(calls.onchangeschange).toHaveBeenLastCalledWith([
      { rowKey: "a", columnKey: "replicas", previous: 3, value: 9 },
    ]);
    expect(calls.onselect).toHaveBeenLastCalledWith("a", rows[0]);
    expect(calls.onfilterschange).toHaveBeenLastCalledWith({ region: "fra" });
    expect(warnings()).toEqual([]);
  });

  it("TreeView reports expanded and selected state, and rename and reorder as discrete events", async () => {
    const calls = { onexpandedchange: vi.fn(), onselectedchange: vi.fn(), onrename: vi.fn(), onreorder: vi.fn() };
    const root = render(TreeView, { label: "Files", nodes, ...calls });
    const projects = root.querySelector<HTMLElement>('[data-tree-id="projects"]')!;

    press(projects, "ArrowRight");
    expect(calls.onexpandedchange).toHaveBeenLastCalledWith(["projects"]);
    press(projects, " ");
    expect(calls.onselectedchange).toHaveBeenLastCalledWith("projects");

    press(projects, "F2");
    await tick();
    const editor = root.querySelector<HTMLInputElement>('[data-tree-rename="projects"]')!;
    type(editor, "Workspace");
    press(editor, "Enter");
    expect(calls.onrename).toHaveBeenLastCalledWith(nodes[0], "Workspace");

    const dataTransfer = { effectAllowed: "none", dropEffect: "none", setData: vi.fn(), getData: () => "projects/index" };
    const target = root.querySelector<HTMLElement>('[data-tree-id="projects/utils"]')!;
    for (const [element, name] of [[root.querySelector('[data-tree-id="projects/index"]')!, "dragstart"], [target, "dragover"], [target, "drop"]] as const) {
      const event = new Event(name, { bubbles: true, cancelable: true });
      Object.defineProperty(event, "dataTransfer", { value: dataTransfer });
      element.dispatchEvent(event);
    }
    expect(calls.onreorder).toHaveBeenLastCalledWith(
      expect.objectContaining({ sourceId: "projects/index", targetId: "projects/utils" }),
    );
    expect(warnings()).toEqual([]);
  });

  it("JsonTree and XmlTree report expansion and the selected path", () => {
    for (const [component, value, path] of [
      [JsonTree, { user: { name: "Ada" } }, "user"],
      [XmlTree, "<user><name>Ada</name></user>", "user"],
    ] as const) {
      const onexpandedchange = vi.fn();
      const onselectedchange = vi.fn();
      const root = render(component, { value, onexpandedchange, onselectedchange });
      const node = root.querySelector<HTMLElement>(`[data-tree-id="${path}"]`)!;
      press(node, "ArrowRight");
      expect(onexpandedchange).toHaveBeenLastCalledWith([path]);
      node.click();
      flushSync();
      expect(onselectedchange).toHaveBeenLastCalledWith(path);
    }
    expect(warnings()).toEqual([]);
  });

  it("DocumentViewer reports its mode through onmodechange and an activated document through onselect", () => {
    const onmodechange = vi.fn();
    const onselect = vi.fn();
    const root = render(DocumentViewer, { documents, onmodechange, onselect });
    const tab = [...root.querySelectorAll<HTMLButtonElement>('[role="tab"]')].find((item) => item.textContent?.trim() === "Raw")!;
    tab.click();
    flushSync();
    expect(onmodechange).toHaveBeenLastCalledWith("raw");
    root.querySelectorAll<HTMLButtonElement>('[role="option"]')[1]!.click();
    flushSync();
    expect(onselect).toHaveBeenLastCalledWith(documents[1], 1);
    // The viewer drives Base Tabs through onvaluechange, so Tabs never warns.
    expect(warnings()).toEqual([]);
  });

  it("CodeField reports every edit through onvaluechange", () => {
    const onvaluechange = vi.fn();
    const root = render(CodeField, { label: "Template", onvaluechange });
    type(root.querySelector<HTMLInputElement>("[data-code-input]")!, "Hello");
    expect(onvaluechange).toHaveBeenLastCalledWith("Hello");
    expect(warnings()).toEqual([]);
  });

  it("Graph reports its focused node and collapsed communities as state, and interactions as a discrete event", () => {
    const onfocusednodechange = vi.fn();
    const oninteraction = vi.fn();
    const root = render(Graph, { data: graph, renderer: "svg", onfocusednodechange, oninteraction });
    root.querySelector<SVGCircleElement>('[data-node-id="gateway"]')!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    flushSync();
    expect(onfocusednodechange).toHaveBeenLastCalledWith("gateway");
    expect(oninteraction).toHaveBeenLastCalledWith({ kind: "node", action: "click", id: "gateway" });
    expect(warnings()).toEqual([]);
  });
});

describe("the Data Kit's deprecated callback names", () => {
  it("still fire for one release and warn once each in development", async () => {
    const table = {
      onSort: vi.fn(),
      onFilters: vi.fn(),
      onPage: vi.fn(),
      onChanges: vi.fn(),
      onRowSelect: vi.fn(),
    };
    for (let pass = 0; pass < 2; pass += 1) {
      const root = render(Table, { caption: "Regions", columns, rows, rowKey: "id", editable: true, pageSize: 1, ...table });
      await driveTable(root);
    }
    for (const callback of Object.values(table)) expect(callback).toHaveBeenCalled();

    const tree = { onExpanded: vi.fn(), onSelected: vi.fn() };
    for (let pass = 0; pass < 2; pass += 1) {
      const root = render(TreeView, { label: "Files", nodes, ...tree });
      const projects = root.querySelector<HTMLElement>('[data-tree-id="projects"]')!;
      press(projects, "ArrowRight");
      press(projects, " ");
    }
    expect(tree.onExpanded).toHaveBeenCalledWith(["projects"]);
    expect(tree.onSelected).toHaveBeenCalledWith("projects");

    const onPath = vi.fn();
    const json = render(JsonTree, { value: { user: { name: "Ada" } }, onPath });
    json.querySelector<HTMLElement>('[data-tree-id="user"]')!.click();
    flushSync();
    expect(onPath).toHaveBeenCalledWith("user");

    const documentCalls = { onSelected: vi.fn(), onMode: vi.fn() };
    const viewer = render(DocumentViewer, { documents, ...documentCalls });
    [...viewer.querySelectorAll<HTMLButtonElement>('[role="tab"]')].at(-1)!.click();
    viewer.querySelectorAll<HTMLButtonElement>('[role="option"]')[1]!.click();
    flushSync();
    expect(documentCalls.onMode).toHaveBeenCalledWith("raw");
    expect(documentCalls.onSelected).toHaveBeenCalledWith(documents[1], 1);

    const onValue = vi.fn();
    const field = render(CodeField, { label: "Template", onValue });
    type(field.querySelector<HTMLInputElement>("[data-code-input]")!, "Hi");
    expect(onValue).toHaveBeenCalledWith("Hi");

    const graphCalls = { onFocusedNode: vi.fn(), onInteraction: vi.fn() };
    const network = render(Graph, { data: graph, renderer: "svg", ...graphCalls });
    network.querySelector<SVGCircleElement>('[data-node-id="api"]')!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    flushSync();
    expect(graphCalls.onFocusedNode).toHaveBeenCalledWith("api");
    expect(graphCalls.onInteraction).toHaveBeenCalled();

    const expected = [
      "[reddb Table] onChanges is deprecated and is removed in the next release. Use onchangeschange.",
      "[reddb Table] onSort is deprecated and is removed in the next release. Use onsortchange.",
      "[reddb Table] onFilters is deprecated and is removed in the next release. Use onfilterschange.",
      "[reddb Table] onPage is deprecated and is removed in the next release. Use onpagechange.",
      "[reddb Table] onRowSelect is deprecated and is removed in the next release. Use onselect.",
      "[reddb TreeView] onExpanded is deprecated and is removed in the next release. Use onexpandedchange.",
      "[reddb TreeView] onSelected is deprecated and is removed in the next release. Use onselectedchange.",
      "[reddb JsonTree] onPath is deprecated and is removed in the next release. Use onselectedchange.",
      "[reddb DocumentViewer] onSelected is deprecated and is removed in the next release. Use onselect.",
      "[reddb DocumentViewer] onMode is deprecated and is removed in the next release. Use onmodechange.",
      "[reddb CodeField] onValue is deprecated and is removed in the next release. Use onvaluechange.",
      "[reddb Graph] onFocusedNode is deprecated and is removed in the next release. Use onfocusednodechange.",
      "[reddb Graph] onInteraction is deprecated and is removed in the next release. Use oninteraction.",
    ];
    // Each alias warns exactly once, however often it is used.
    expect([...warnings()].sort()).toEqual([...expected].sort());
  });

  it("keeps KpiCard's tone=\"brand\" as a working alias that warns once", () => {
    for (let pass = 0; pass < 2; pass += 1) {
      const card = render(KpiCard, { label: "Adoption", value: "72%", tone: "brand" }).querySelector<HTMLElement>("[data-kpi-card]")!;
      expect(card.className).toContain("bg-[var(--reddb-color-primary)]");
    }
    expect(warnings()).toHaveLength(1);
    expect(warnings()[0]).toMatch(/\[reddb KpiCard\] tone="brand" is deprecated.*a tone \(neutral, info, success, warning, danger\)/);
  });
});
