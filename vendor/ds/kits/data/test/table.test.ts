import { afterEach, describe, expect, it, vi } from "vitest";
import { createRawSnippet, flushSync, mount, tick, unmount } from "svelte";
import Table from "../src/Table.svelte";
import {
  applyChanges,
  filterRows,
  paginateRows,
  sortRows,
  stageChange,
  valueWithChanges,
  virtualWindow,
  type TableActionsContext,
  type TableChange,
  type TableColumn,
} from "../src/table";

interface Deployment extends Record<string, unknown> {
  id: string;
  region: string;
  replicas: number;
}

const columns: readonly TableColumn<Deployment>[] = [
  { key: "region", header: "Region", sortable: true, filterable: true },
  { key: "replicas", header: "Replicas", sortable: true, editable: true },
];

const rows: readonly Deployment[] = [
  { id: "a", region: "São Paulo", replicas: 3 },
  { id: "b", region: "Frankfurt", replicas: 2 },
  { id: "c", region: "Frankfurt", replicas: 5 },
  { id: "d", region: "Singapore", replicas: 1 },
];

const mounted: Array<{ instance: Record<string, unknown>; target: HTMLElement }> = [];

function render(props: Record<string, unknown>): HTMLElement {
  const target = document.createElement("div");
  document.body.append(target);
  const instance = mount(Table, { target, props }) as Record<string, unknown>;
  mounted.push({ instance, target });
  flushSync();
  return target;
}

function press(element: Element, key: string): KeyboardEvent {
  const event = new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true });
  element.dispatchEvent(event);
  flushSync();
  return event;
}

afterEach(() => {
  for (const entry of mounted.splice(0)) {
    unmount(entry.instance);
    entry.target.remove();
  }
});

describe("the rich Table's pure model", () => {
  it("sorts stably without mutating caller rows, filters per column, and paginates", () => {
    const sorted = sortRows(rows, columns, { key: "region", direction: "ascending" });

    expect(sorted.map((row) => row.id)).toEqual(["b", "c", "a", "d"]);
    expect(rows.map((row) => row.id)).toEqual(["a", "b", "c", "d"]);
    expect(filterRows(sorted, columns, { region: "FRA" }).map((row) => row.id)).toEqual([
      "b",
      "c",
    ]);
    expect(paginateRows(sorted, { page: 2, pageSize: 2 })).toMatchObject({
      page: 2,
      pageCount: 2,
      total: 4,
      rows: [rows[0], rows[3]],
    });
  });

  it("computes a bounded, overscanned virtualization window", () => {
    expect(
      virtualWindow(Array.from({ length: 100 }, (_, index) => index), {
        scrollTop: 400,
        viewportHeight: 200,
        rowHeight: 40,
        overscan: 2,
      }),
    ).toMatchObject({
      start: 8,
      end: 17,
      offsetTop: 320,
      offsetBottom: 3_320,
      totalHeight: 4_000,
      items: [8, 9, 10, 11, 12, 13, 14, 15, 16],
    });
  });

  it("stages one latest change per cell, removes reversions, and applies a change-set", () => {
    let changes: readonly TableChange[] = [];
    changes = stageChange(changes, {
      rowKey: "a",
      columnKey: "replicas",
      previous: 3,
      value: 4,
    });
    changes = stageChange(changes, {
      rowKey: "a",
      columnKey: "replicas",
      previous: 3,
      value: 6,
    });

    expect(changes).toEqual([
      { rowKey: "a", columnKey: "replicas", previous: 3, value: 6 },
    ]);
    expect(applyChanges(rows, changes, (row) => row.id)[0]?.replicas).toBe(6);
    expect(
      stageChange(changes, {
        rowKey: "a",
        columnKey: "replicas",
        previous: 3,
        value: 3,
      }),
    ).toEqual([]);
    expect(
      valueWithChanges(3, [{ rowKey: "a", columnKey: "replicas", previous: 3, value: null }], "a", "replicas"),
    ).toBeNull();
  });
});

describe("the rich Table component", () => {
  it("exposes native table relationships and roving two-dimensional cell navigation", () => {
    const root = render({ caption: "Deployments", columns, rows, rowKey: "id" });
    const table = root.querySelector("table")!;
    const cells = [...root.querySelectorAll<HTMLElement>("tbody [data-table-cell]")];

    expect(table.querySelector("caption")?.textContent).toBe("Deployments");
    expect([...table.querySelectorAll("thead th")].map((cell) => cell.getAttribute("scope"))).toEqual([
      "col",
      "col",
    ]);
    expect(cells.map((cell) => cell.tabIndex)).toEqual([0, -1, -1, -1, -1, -1, -1, -1]);

    cells[0]!.focus();
    expect(press(cells[0]!, "ArrowRight").defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(cells[1]);
    press(cells[1]!, "ArrowDown");
    expect(document.activeElement).toBe(cells[3]);
    press(cells[3]!, "Home");
    expect(document.activeElement).toBe(cells[2]);
  });

  it("keeps read-only cells plain and stages editable cells through callbacks", async () => {
    const onchangeschange = vi.fn();
    const root = render({
      caption: "Deployments",
      columns,
      rows,
      rowKey: "id",
      editable: true,
      onchangeschange,
    });
    const cells = [...root.querySelectorAll<HTMLElement>("tbody [data-table-cell]")];

    expect(root.querySelector("input[data-table-editor]")).toBeNull();
    press(cells[1]!, "Enter");
    await tick();
    const editor = root.querySelector<HTMLInputElement>("input[data-table-editor]")!;
    expect(editor).not.toBeNull();
    expect(document.activeElement).toBe(editor);
    editor.value = "7";
    editor.dispatchEvent(new Event("input", { bubbles: true }));
    press(editor, "Enter");

    expect(onchangeschange).toHaveBeenLastCalledWith([
      { rowKey: "a", columnKey: "replicas", previous: 3, value: 7 },
    ]);
    expect(root.querySelector("[data-table-pending]")?.textContent).toBe("7");

    press(cells[1]!, "Enter");
    await tick();
    const nextEditor = root.querySelector<HTMLInputElement>("input[data-table-editor]")!;
    nextEditor.value = "8";
    nextEditor.dispatchEvent(new Event("input", { bubbles: true }));
    const filter = root.querySelector<HTMLInputElement>('input[aria-label="Filter Region"]')!;
    filter.focus();
    flushSync();
    expect(document.activeElement).toBe(filter);
    expect(onchangeschange).toHaveBeenLastCalledWith([
      { rowKey: "a", columnKey: "replicas", previous: 3, value: 8 },
    ]);

    const plain = render({ caption: "Read only", columns, rows, rowKey: "id" });
    press(plain.querySelectorAll("tbody [data-table-cell]")[1]!, "Enter");
    expect(plain.querySelector("input[data-table-editor]")).toBeNull();
  });

  it("offers filters and a keyboard-native column visibility picker", () => {
    const root = render({ caption: "Deployments", columns, rows, rowKey: "id" });
    const filter = root.querySelector<HTMLInputElement>('input[aria-label="Filter Region"]')!;
    filter.value = "sing";
    filter.dispatchEvent(new Event("input", { bubbles: true }));
    flushSync();
    expect(root.querySelectorAll("tbody tr[data-table-row]")).toHaveLength(1);

    const replicas = root.querySelector<HTMLInputElement>(
      'input[aria-label="Show Replicas column"]',
    )!;
    replicas.click();
    flushSync();
    expect([...root.querySelectorAll("thead th")].map((cell) => cell.textContent?.trim())).toEqual([
      "Region",
    ]);
  });

  it("drops the column visibility picker, and an empty tool row, when a caller turns it off", () => {
    const plainColumns = columns.map(({ filterable: _filterable, ...column }) => column);
    const bare = render({
      caption: "Response headers",
      columns: plainColumns,
      rows,
      rowKey: "id",
      columnVisibility: false,
    });
    expect(bare.querySelector("[data-column-visibility]")).toBeNull();
    expect(bare.querySelector("[data-table-tools]")).toBeNull();

    const filtered = render({ caption: "Deployments", columns, rows, rowKey: "id", columnVisibility: false });
    expect(filtered.querySelector("[data-column-visibility]")).toBeNull();
    expect(filtered.querySelector('input[aria-label="Filter Region"]')).not.toBeNull();
  });

  it("renders caller actions without built-in tools and supplies current filtered rows and edits", () => {
    const snapshots: TableActionsContext[] = [];
    const actions = createRawSnippet<[TableActionsContext]>((context) => ({
      render: () => '<button type="button" data-custom-action>Export</button>',
      setup: (element) => { element.addEventListener("click", () => snapshots.push(context())); },
    }));
    const changes = [{ rowKey: "b", columnKey: "replicas", previous: 2, value: 8 }];
    const root = render({ caption: "Deployments", columns, rows, rowKey: "id", pageSize: 1, selectedRow: "b", changes, actions });
    const filter = root.querySelector<HTMLInputElement>('input[aria-label="Filter Region"]')!;
    filter.value = "Frankfurt"; filter.dispatchEvent(new Event("input", { bubbles: true })); flushSync();
    root.querySelector<HTMLButtonElement>("[data-custom-action]")!.click();
    expect(snapshots[0]!.rows.map(row => row.id)).toEqual(["b", "c"]);
    expect(snapshots[0]!.pageRows.map(row => row.id)).toEqual(["b"]);
    expect(snapshots[0]!.selectedRow).toBe("b"); expect(snapshots[0]!.changes).toEqual(changes);
    const bare = render({ caption: "Empty", columns: [], rows: [], columnVisibility: false, actions });
    expect(bare.querySelector('[data-table-actions][aria-label="Empty actions"]')).not.toBeNull();
    bare.querySelector<HTMLButtonElement>("[data-custom-action]")!.click();
    expect(snapshots[1]!.rows).toEqual([]);
  });

  it("renders cleanly through every appearance axis in editable and read-only modes", () => {
    for (const theme of ["base", "editorial"]) {
      for (const scheme of ["light", "dark"]) {
        for (const density of ["tiny", "compact", "comfortable", "spacious"]) {
          for (const editable of [false, true]) {
            const root = render({
              caption: `${theme}-${scheme}-${density}-${editable}`,
              columns,
              rows,
              rowKey: "id",
              editable,
            });
            const scope = root.firstElementChild as HTMLElement;
            scope.dataset.theme = theme;
            scope.dataset.colorScheme = scheme;
            scope.dataset.density = density;
            expect(scope.querySelector("table")?.rows).toHaveLength(5);
            expect(scope.querySelectorAll("input[data-table-editor]")).toHaveLength(0);
          }
        }
      }
    }
  });

  it("right-aligns numeric columns with tabular numerals and renders caller-owned cells", () => {
    const cell = createRawSnippet((context: () => { column: { key: string }; text: string }) => ({
      render: () => `<em data-cell-key="${context().column.key}">${context().text}</em>`,
    }));
    const root = render({
      caption: "Deployments",
      columns: [columns[0], { ...columns[1], align: "end" }],
      rows,
      rowKey: "id",
      cell,
      captionHidden: true,
      columnVisibility: false,
    });
    const headers = [...root.querySelectorAll<HTMLElement>("thead th")];
    const firstRow = [...root.querySelectorAll<HTMLElement>("tbody tr")[0]!.querySelectorAll("td")];

    expect(root.querySelector("caption")?.className).toBe("sr-only");
    expect(root.querySelector("[data-column-visibility]")).toBeNull();
    // The filterable Region column still keeps the tools row.
    expect(root.querySelector("[data-table-tools]")).not.toBeNull();
    expect(headers[0]!.className).not.toContain("text-end");
    expect(headers[1]!.className).toContain("text-end tabular-nums");
    expect(firstRow[1]!.className).toContain("text-end tabular-nums");
    expect(firstRow[1]!.querySelector("em[data-cell-key='replicas']")?.textContent).toBe("3");
  });

  it("selects a row by pointer or keyboard and reports it through a controlled key", () => {
    const onselect = vi.fn();
    const empty = createRawSnippet(() => ({ render: () => "<span data-empty>Nothing here</span>" }));
    const root = render({
      caption: "Deployments",
      columns,
      rows,
      rowKey: "id",
      selectedRow: "b",
      onselect,
      empty,
    });
    const rowElements = [...root.querySelectorAll<HTMLElement>("tbody tr[data-table-row]")];

    expect(rowElements.map((row) => row.getAttribute("aria-selected"))).toEqual([
      "false",
      "true",
      "false",
      "false",
    ]);
    expect(rowElements[1]!.className).toContain("bg-foreground/10");

    rowElements[2]!.querySelector<HTMLElement>("td")!.click();
    expect(onselect).toHaveBeenLastCalledWith("c", rows[2]);
    press(rowElements[3]!.querySelector("td")!, "Enter");
    expect(onselect).toHaveBeenLastCalledWith("d", rows[3]);
    press(rowElements[0]!.querySelector("td")!, " ");
    expect(onselect).toHaveBeenLastCalledWith("a", rows[0]);

    const filter = root.querySelector<HTMLInputElement>('input[aria-label="Filter Region"]')!;
    filter.value = "nowhere";
    filter.dispatchEvent(new Event("input", { bubbles: true }));
    flushSync();
    expect(root.querySelector("[data-table-empty] [data-empty]")?.textContent).toBe("Nothing here");

    const plain = render({ caption: "Plain", columns, rows, rowKey: "id" });
    expect(plain.querySelector("tbody tr")?.hasAttribute("aria-selected")).toBe(false);
  });
});

describe("the rich Table's presentation seams", () => {
  it("aligns quantity columns to the end with tabular numerals and shows filter examples", () => {
    const target = render({
      caption: "Deployments",
      rowKey: "id",
      columns: [
        { key: "region", header: "Region", filterable: true, filterPlaceholder: "e.g. Frankfurt" },
        { key: "replicas", header: "Replicas", sortable: true, align: "end" },
      ],
      rows,
    });

    const [regionHead, replicasHead] = target.querySelectorAll<HTMLElement>("th");
    const replicasCell = target.querySelector<HTMLElement>('[data-cell-column="1"]')!;
    expect(regionHead?.dataset.align).toBe("start");
    expect(replicasHead?.dataset.align).toBe("end");
    expect(replicasHead?.className).toContain("text-end");
    expect(replicasCell.className).toContain("text-end");
    expect(replicasCell.className).toContain("tabular-nums");
    expect(target.querySelector<HTMLInputElement>('input[type="search"]')?.placeholder).toBe(
      "e.g. Frankfurt",
    );
    expect(target.querySelector<HTMLInputElement>('input[type="search"]')?.className).toContain(
      "border-control-edge",
    );
  });

  it("renders read-only cells through a caller snippet and names the empty result", async () => {
    const cell = createRawSnippet<[{ column: { key: string }; text: string }]>((context) => ({
      render: () => `<mark data-custom-cell="${context().column.key}">${context().text}</mark>`,
    }));
    const target = render({
      caption: "Deployments",
      rowKey: "id",
      columns: [{ key: "region", header: "Region", filterable: true }, ...columns.slice(1)],
      rows,
      cell,
      emptyMessage: "No deployments match these filters.",
    });

    expect(target.querySelectorAll('[data-custom-cell="region"]')).toHaveLength(4);
    expect(target.querySelector('[data-custom-cell="region"]')?.textContent).toBe("São Paulo");

    const filter = target.querySelector<HTMLInputElement>('input[type="search"]')!;
    filter.value = "Atlantis";
    filter.dispatchEvent(new Event("input", { bubbles: true }));
    flushSync();
    await tick();

    expect(target.querySelector("[data-table-empty]")?.textContent).toBe(
      "No deployments match these filters.",
    );
  });
});
