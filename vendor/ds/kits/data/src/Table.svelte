<!-- The Data Kit's state-local rich grid; all row data and mutations remain caller-owned. -->
<script module lang="ts">
  export {
    applyChanges,
    filterRows,
    paginateRows,
    sortRows,
    stageChange,
    valueWithChanges,
    virtualWindow,
  } from "./table";
  export type {
    PaginatedRows,
    PaginationOptions,
    SortDirection,
    TableActionsContext,
    TableCellContext,
    TableChange,
    TableColumn,
    TableColumnAlign,
    TableFilters,
    TableRow,
    TableRowKey,
    TableSort,
    VirtualWindow,
    VirtualWindowOptions,
  } from "./table";
</script>

<script lang="ts">
 import type {TableContract} from "./core/contracts/table";
  import { Button, Checkbox, Input } from "@reddb-io/design-system/base";
  import { tick, type Snippet } from "svelte";
  import { quietControl, warnDeprecated } from "@reddb-io/design-system/base";
  import {
    filterRows,
    paginateRows,
    sortRows,
    stageChange,
    valueWithChanges,
    virtualWindow,
    type TableActionsContext,
    type TableCellContext,
    type TableChange,
    type TableColumn,
    type TableFilters,
    type TableRow,
    type TableRowKey,
    type TableSort,
    type VirtualWindowOptions,
  } from "./table";

  interface TableVirtualization extends Omit<VirtualWindowOptions, "scrollTop"> {}

  interface Props extends TableContract {
    /** Receives a complete immutable staged change-set after each edit. */
    onchangeschange?: (changes: readonly TableChange[]) => void;
    /** @deprecated Renamed `onchangeschange` (ADR 0026); removed next release. */
    onChanges?: (changes: readonly TableChange[]) => void;
    /** Reports the sort after a sortable header cycles it. */
    onsortchange?: (sort: TableSort | null) => void;
    /** @deprecated Renamed `onsortchange` (ADR 0026); removed next release. */
    onSort?: (sort: TableSort | null) => void;
    /** Reports every column filter after one changes. */
    onfilterschange?: (filters: TableFilters) => void;
    /** @deprecated Renamed `onfilterschange` (ADR 0026); removed next release. */
    onFilters?: (filters: TableFilters) => void;
    /** Reports the page the reader moved to. */
    onpagechange?: (page: number) => void;
    /** @deprecated Renamed `onpagechange` (ADR 0026); removed next release. */
    onPage?: (page: number) => void;
    /** Caller-owned buttons, links, menus or controls at the end of the toolbar; receives filtered rows, current page, selection and staged edits. Also renders when built-in filters and column visibility are absent. */
    actions?: Snippet<[TableActionsContext]>;
    /** Caller-owned rendering for read-only cells; editing keeps the Table's own editor. */
    cell?: Snippet<[TableCellContext]>;
    /** Caller-owned content for a grid with no rows after filtering. */
    empty?: Snippet;
    /**
     * Item activation (ADR 0026's sanctioned `onselect`): receives the row a
     * pointer or Enter/Space on a read-only cell activates.
     */
    onselect?: (key: TableRowKey, row: TableRow) => void;
    /** @deprecated Renamed `onselect` (ADR 0026); removed next release. */
    onRowSelect?: (key: TableRowKey, row: TableRow) => void;
    /** Extra classes, merged onto the `table` element itself, inside its scroll region. */
    class?: string;
  }

  let {
    caption,
    columns,
    rows,
    rowKey,
    editable = false,
    changes: controlledChanges,
    onchangeschange,
    onChanges,
    sort: controlledSort,
    onsortchange,
    onSort,
    filters: controlledFilters,
    onfilterschange,
    onFilters,
    page: controlledPage,
    pageSize = 25,
    onpagechange,
    onPage,
    virtualize = false,
    captionHidden = false,
    columnVisibility = true,
    actions,
    cell,
    empty,
    emptyMessage = "No rows",
    selectedRow = null,
    onselect,
    onRowSelect,
    class: className = "",
    containerClass = "",
  }: Props = $props();

  // Each deprecated camelCase name still fires beside its canonical callback
  // for one release and warns once in development (ADR 0026).
  $effect(() => {
    if (onChanges) warnDeprecated("Table", "onChanges", "onchangeschange");
    if (onSort) warnDeprecated("Table", "onSort", "onsortchange");
    if (onFilters) warnDeprecated("Table", "onFilters", "onfilterschange");
    if (onPage) warnDeprecated("Table", "onPage", "onpagechange");
    if (onRowSelect) warnDeprecated("Table", "onRowSelect", "onselect");
  });

  function reportPage(page: number): void {
    onpagechange?.(page);
    onPage?.(page);
  }

  let localChanges = $state<readonly TableChange[]>([]);
  let localSort = $state<TableSort | null>(null);
  let localFilters = $state<Record<string, string | undefined>>({});
  let localPage = $state(1);
  let visibility = $state<Record<string, boolean>>({});
  let scrollTop = $state(0);
  // What a row actually renders at. Rows are sized by Density roles, so the
  // caller's `rowHeight` is only the first-paint estimate: the window and its
  // spacers follow the measured row, or they drift at every stop but one.
  let measuredRowHeight = $state<number>();
  let active = $state({ row: 0, column: 0 });
  let grid = $state<HTMLTableElement>();
  let editor = $state<HTMLInputElement>();
  let editing = $state<{
    rowKey: TableRowKey;
    columnKey: string;
    row: TableRow;
    rowPosition: number;
    columnPosition: number;
    previous: unknown;
    input: string;
  } | null>(null);

  const activeChanges = $derived(controlledChanges ?? localChanges);
  const activeSort = $derived(controlledSort === undefined ? localSort : controlledSort);
  const activeFilters = $derived(controlledFilters ?? localFilters);
  const requestedPage = $derived(controlledPage ?? localPage);
  const visibleColumns = $derived(columns.filter((column) => visibility[column.key] !== false));
  const filteredRows = $derived(filterRows(rows, columns, activeFilters));
  const sortedRows = $derived(sortRows(filteredRows, columns, activeSort));
  const pagination = $derived(paginateRows(sortedRows, { page: requestedPage, pageSize }));
  const windowed = $derived(
    virtualize
      ? virtualWindow(pagination.rows, {
          ...virtualize,
          rowHeight: measuredRowHeight ?? virtualize.rowHeight,
          scrollTop,
        })
      : {
          items: pagination.rows,
          start: 0,
          end: pagination.rows.length,
          offsetTop: 0,
          offsetBottom: 0,
          totalHeight: pagination.rows.length,
        },
  );

  function keyFor(row: TableRow, index: number): TableRowKey {
    if (typeof rowKey === "function") return rowKey(row, index);
    if (typeof rowKey === "string") {
      const value = row[rowKey];
      if (typeof value === "string" || typeof value === "number") return value;
    }
    return index;
  }

  const filterableColumns = $derived(columns.filter((column) => column.filterable));
  const selectable = $derived(onselect !== undefined || onRowSelect !== undefined);

  function selectRow(row: TableRow, rowPosition: number): void {
    const key = keyFor(row, pagination.start + windowed.start + rowPosition);
    onselect?.(key, row);
    onRowSelect?.(key, row);
  }

  function textFor(column: TableColumn, value: unknown, row: TableRow): string {
    return column.format ? column.format(value, row) : String(value ?? "");
  }

  function canEdit(row: TableRow, column: TableColumn): boolean {
    if (column.editable !== true) return false;
    return typeof editable === "function" ? editable(row, column) : editable;
  }

  function hasChange(rowKeyValue: TableRowKey, columnKey: string): boolean {
    return activeChanges.some(
      (change) => change.rowKey === rowKeyValue && change.columnKey === columnKey,
    );
  }

  function setFilters(columnKey: string, query: string): void {
    const next = { ...activeFilters, [columnKey]: query };
    localFilters = next;
    localPage = 1;
    onfilterschange?.(next);
    onFilters?.(next);
    if (controlledPage !== undefined) reportPage(1);
  }

  function toggleColumn(columnKey: string): void {
    visibility = { ...visibility, [columnKey]: visibility[columnKey] === false };
    active = { row: active.row, column: Math.min(active.column, visibleColumns.length - 1) };
  }

  function toggleSort(column: TableColumn): void {
    if (!column.sortable) return;
    let next: TableSort | null;
    if (activeSort?.key !== column.key) next = { key: column.key, direction: "ascending" };
    else if (activeSort.direction === "ascending") {
      next = { key: column.key, direction: "descending" };
    } else next = null;
    localSort = next;
    localPage = 1;
    onsortchange?.(next);
    onSort?.(next);
  }

  function sortValue(column: TableColumn): "ascending" | "descending" | "none" {
    return activeSort?.key === column.key ? activeSort.direction : "none";
  }

  function focusCell(row: number, column: number): void {
    const rowCount = windowed.items.length;
    if (rowCount === 0 || visibleColumns.length === 0) return;
    const next = {
      row: Math.min(rowCount - 1, Math.max(0, row)),
      column: Math.min(visibleColumns.length - 1, Math.max(0, column)),
    };
    active = next;
    grid
      ?.querySelector<HTMLElement>(
        `[data-cell-row="${next.row}"][data-cell-column="${next.column}"]`,
      )
      ?.focus();
  }

  function beginEdit(
    row: TableRow,
    rowPosition: number,
    column: TableColumn,
    columnPosition: number,
    value: unknown,
  ): void {
    if (!canEdit(row, column)) return;
    const dataIndex = pagination.start + windowed.start + rowPosition;
    editing = {
      rowKey: keyFor(row, dataIndex),
      columnKey: column.key,
      row,
      rowPosition,
      columnPosition,
      previous: row[column.key],
      input: textFor(column, value, row),
    };
    void tick().then(() => {
      editor?.focus();
      editor?.select();
    });
  }

  function finishEdit(commit: boolean, restoreFocus = true): void {
    if (!editing) return;
    const current = editing;
    if (commit) {
      const column = columns.find((candidate) => candidate.key === current.columnKey)!;
      let value: unknown = current.input;
      if (column.parse) value = column.parse(current.input, current.previous, current.row);
      else if (typeof current.previous === "number" && current.input.trim() !== "") {
        const numeric = Number(current.input);
        if (Number.isFinite(numeric)) value = numeric;
      }
      const next = stageChange(activeChanges, {
        rowKey: current.rowKey,
        columnKey: current.columnKey,
        previous: current.previous,
        value,
      });
      localChanges = next;
      onchangeschange?.(next);
      onChanges?.(next);
    }
    editing = null;
    if (restoreFocus) focusCell(current.rowPosition, current.columnPosition);
  }

  function handleCellKeydown(
    event: KeyboardEvent,
    row: TableRow,
    rowPosition: number,
    column: TableColumn,
    columnPosition: number,
    value: unknown,
  ): void {
    if (event.target !== event.currentTarget) return;
    let destination: { row: number; column: number } | undefined;
    if (event.key === "ArrowRight") destination = { row: rowPosition, column: columnPosition + 1 };
    else if (event.key === "ArrowLeft") destination = { row: rowPosition, column: columnPosition - 1 };
    else if (event.key === "ArrowDown") destination = { row: rowPosition + 1, column: columnPosition };
    else if (event.key === "ArrowUp") destination = { row: rowPosition - 1, column: columnPosition };
    else if (event.key === "Home") destination = { row: rowPosition, column: 0 };
    else if (event.key === "End") {
      destination = { row: rowPosition, column: visibleColumns.length - 1 };
    } else if (event.key === "Enter" || event.key === "F2") {
      if (canEdit(row, column)) {
        event.preventDefault();
        beginEdit(row, rowPosition, column, columnPosition, value);
      } else if (event.key === "Enter" && selectable) {
        event.preventDefault();
        selectRow(row, rowPosition);
      }
      return;
    } else if (event.key === " " && selectable && !canEdit(row, column)) {
      event.preventDefault();
      selectRow(row, rowPosition);
      return;
    }
    if (destination) {
      event.preventDefault();
      focusCell(destination.row, destination.column);
    }
  }

  function handleEditorKeydown(event: KeyboardEvent): void {
    if (event.key === "Enter") {
      event.preventDefault();
      finishEdit(true);
    } else if (event.key === "Escape") {
      event.preventDefault();
      finishEdit(false);
    }
  }

  function alignClass(column: TableColumn): string {
    return column.align === "end" ? "text-end tabular-nums" : "text-start";
  }

  $effect(() => {
    // Re-run as the window moves: the row observed last may have left the DOM.
    void windowed.start;
    if (!virtualize || !grid || typeof ResizeObserver === "undefined") return;
    const row = grid.querySelector<HTMLTableRowElement>("[data-table-row]");
    if (!row) return;
    const measure = () => {
      const height = row.getBoundingClientRect().height;
      if (height > 0) measuredRowHeight = height;
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(row);
    return () => observer.disconnect();
  });

  function goToPage(page: number): void {
    const next = Math.min(pagination.pageCount, Math.max(1, page));
    localPage = next;
    active = { row: 0, column: 0 };
    reportPage(next);
  }
</script>

<div
  data-data-table
  class={`grid gap-[var(--reddb-spatial-layout-gap-sm)] text-foreground ${containerClass}`.trim()}
>
  {#if filterableColumns.length > 0 || columnVisibility || actions}
  <div class="flex flex-wrap items-end gap-[var(--reddb-spatial-layout-gap-sm)]" data-table-tools>
    {#each filterableColumns as column (column.key)}
      <label class="grid gap-[var(--reddb-spatial-gap-sm)] text-sm font-medium">
        <span>Filter {column.header}</span>
        <Input
          type="search"
          aria-label={`Filter ${column.header}`}
          placeholder={column.filterPlaceholder}
          value={activeFilters[column.key] ?? ""}
          oninput={(event) => setFilters(column.key, event.currentTarget.value)}
        />
      </label>
    {/each}

    {#if columnVisibility}
    <details class="relative text-sm [&:not([open])>fieldset]:hidden" data-column-visibility>
      <!-- A <summary> must be the disclosure's own element, so it wears the
           secondary Button's geometry rather than wrapping a Button. -->
      <summary
        class="inline-flex h-[var(--reddb-spatial-control-height-md)] cursor-pointer list-none items-center rounded-md border border-control-edge px-[var(--reddb-spatial-inset-md)] text-foreground hover:border-foreground focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus"
      >Column visibility</summary>
      <fieldset
        class="absolute z-10 grid min-w-max gap-[var(--reddb-spatial-gap-sm)] rounded-md border border-elevation-overlay-border bg-elevation-overlay-surface p-[var(--reddb-spatial-inset-sm)] text-elevation-overlay-foreground shadow-elevation-overlay"
      >
        <legend class="sr-only">Visible columns</legend>
        {#each columns as column (column.key)}
          <Checkbox
            label={column.header}
            aria-label={`Show ${column.header} column`}
            checked={visibility[column.key] !== false}
            disabled={visibility[column.key] !== false && visibleColumns.length === 1}
            oncheckedchange={() => toggleColumn(column.key)}
          />
        {/each}
      </fieldset>
    </details>
    {/if}
    {#if actions}
      <div data-table-actions role="group" aria-label={`${caption} actions`} class="ms-auto flex min-w-0 flex-wrap items-center gap-[var(--reddb-spatial-gap-sm)]">
        {@render actions({ rows: sortedRows, pageRows: pagination.rows, selectedRow, changes: activeChanges, filters: activeFilters, sort: activeSort })}
      </div>
    {/if}
  </div>
  {/if}

  <!-- svelte-ignore a11y_no_noninteractive_tabindex (the named overflow region must be keyboard-scrollable) -->
  <div
    data-table-scroll
    data-row-height={virtualize ? measuredRowHeight : undefined}
    role="region"
    aria-label={`${caption} data`}
    tabindex="0"
    class={`max-w-full overflow-auto rounded-md focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus ${virtualize ? "relative" : ""}`.trim()}
    style:max-height={virtualize ? `${virtualize.viewportHeight}px` : undefined}
    onscroll={(event) => (scrollTop = event.currentTarget.scrollTop)}
  >
    <table
      bind:this={grid}
      role="grid"
      aria-rowcount={pagination.rows.length + 1}
      aria-colcount={visibleColumns.length}
      class={`w-full min-w-max border-collapse text-left text-sm ${className}`.trim()}
      data-table
    >
      <caption
        class={captionHidden
          ? "sr-only"
          : "caption-top px-[var(--reddb-spatial-inset-md)] pb-[var(--reddb-spatial-inset-sm)] text-left font-medium"}
      >{caption}</caption>
      <thead class="sticky top-0 z-10 border-b border-muted bg-background">
        <tr>
          {#each visibleColumns as column (column.key)}
            <th
              scope="col"
              aria-sort={sortValue(column)}
              data-align={column.align ?? "start"}
              class={`px-[var(--reddb-spatial-inset-md)] py-[var(--reddb-spatial-inset-sm)] align-middle font-medium ${alignClass(column)}`.trim()}
            >
              {#if column.sortable}
                <button
                  type="button"
                  class={quietControl({
                    ink: "inherit",
                    class: "-mx-[var(--reddb-spatial-gap-sm)] inline-flex min-h-[var(--reddb-spatial-control-height-sm)] items-center rounded-sm px-[var(--reddb-spatial-gap-sm)]",
                  })}
                  onclick={() => toggleSort(column)}
                >{column.header}</button>
              {:else}
                {column.header}
              {/if}
            </th>
          {/each}
        </tr>
      </thead>
      <tbody>
        {#if windowed.offsetTop > 0}
          <tr aria-hidden="true">
            <td colspan={visibleColumns.length} style:height={`${windowed.offsetTop}px`}></td>
          </tr>
        {/if}
        {#each windowed.items as row, rowPosition (keyFor(row, pagination.start + windowed.start + rowPosition))}
          {@const dataIndex = pagination.start + windowed.start + rowPosition}
          {@const rowKeyValue = keyFor(row, dataIndex)}
          {@const rowSelected = selectable && selectedRow !== null && selectedRow === rowKeyValue}
          <tr
            data-table-row
            data-table-row-selected={rowSelected ? "" : undefined}
            aria-rowindex={dataIndex + 2}
            aria-selected={selectable ? rowSelected : undefined}
            class={`border-b border-muted ${selectable ? "cursor-pointer" : ""} ${rowSelected ? "bg-foreground/10 font-medium" : selectable ? "hover:bg-foreground/8" : ""}`.trim()}
          >
            {#each visibleColumns as column, columnPosition (column.key)}
              {@const original = row[column.key]}
              {@const value = valueWithChanges(original, activeChanges, rowKeyValue, column.key)}
              <td
                data-table-cell
                data-cell-row={rowPosition}
                data-cell-column={columnPosition}
                aria-colindex={columnPosition + 1}
                aria-readonly={!canEdit(row, column)}
                data-align={column.align ?? "start"}
                tabindex={active.row === rowPosition && active.column === columnPosition ? 0 : -1}
                class={`px-[var(--reddb-spatial-inset-md)] py-[var(--reddb-spatial-inset-sm)] align-middle focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-focus ${alignClass(column)}`.trim()}
                onclick={(event) => {
                  active = { row: rowPosition, column: columnPosition };
                  event.currentTarget.focus();
                  if (!canEdit(row, column)) selectRow(row, rowPosition);
                }}
                ondblclick={() => beginEdit(row, rowPosition, column, columnPosition, value)}
                onkeydown={(event) =>
                  handleCellKeydown(
                    event,
                    row,
                    rowPosition,
                    column,
                    columnPosition,
                    value,
                  )}
              >
                {#if editing?.rowKey === rowKeyValue && editing.columnKey === column.key}
                  <Input
                    bind:ref={editor}
                    data-table-editor
                    onclick={(event) => event.stopPropagation()}
                    aria-label={`Edit ${column.header} for row ${dataIndex + 1}`}
                    value={editing.input}
                    oninput={(event) => {
                      if (editing) editing.input = event.currentTarget.value;
                    }}
                    onkeydown={handleEditorKeydown}
                    onblur={() => finishEdit(true, false)}
                    class="h-[var(--reddb-spatial-control-height-sm)] w-full px-[var(--reddb-spatial-inset-sm)]"
                  />
                {:else if hasChange(rowKeyValue, column.key)}
                  <span data-table-pending>{textFor(column, value, row)}</span>
                {:else if cell}
                  {@render cell({ row, column, value, text: textFor(column, value, row) })}
                {:else}
                  {textFor(column, value, row)}
                {/if}
              </td>
            {/each}
          </tr>
        {:else}
          <tr>
            <td
              colspan={visibleColumns.length}
              data-table-empty
              class="px-[var(--reddb-spatial-inset-md)] py-[var(--reddb-spatial-inset-md)] text-center text-ink-muted"
              >{#if empty}{@render empty()}{:else}{emptyMessage}{/if}</td>
          </tr>
        {/each}
        {#if windowed.offsetBottom > 0}
          <tr aria-hidden="true">
            <td colspan={visibleColumns.length} style:height={`${windowed.offsetBottom}px`}></td>
          </tr>
        {/if}
      </tbody>
    </table>
  </div>

  {#if pagination.pageCount > 1}
    <nav aria-label={`${caption} pages`} class="flex items-center justify-end gap-[var(--reddb-spatial-gap-md)] text-sm">
      <Button
        variant="secondary"
        size="sm"
        disabled={pagination.page === 1}
        onclick={() => goToPage(pagination.page - 1)}
      >Previous</Button>
      <span aria-live="polite">Page {pagination.page} of {pagination.pageCount}</span>
      <Button
        variant="secondary"
        size="sm"
        disabled={pagination.page === pagination.pageCount}
        onclick={() => goToPage(pagination.page + 1)}
      >Next</Button>
    </nav>
  {/if}
</div>
