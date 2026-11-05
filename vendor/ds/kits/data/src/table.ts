/** Pure data operations for the Data Kit Table. This module has no Svelte or DOM dependency. */

export type TableRow = Record<string, unknown>;
export type TableRowKey = string | number;
export type SortDirection = "ascending" | "descending";

export interface TableSort {
  key: string;
  direction: SortDirection;
}

export type TableColumnAlign = "start" | "end";

/** What a caller-owned `cell` snippet receives for one read-only body cell. */
export interface TableCellContext<Row extends TableRow = TableRow> {
  column: TableColumn<Row>;
  row: Row;
  /** The displayed value, staged changes included. */
  value: unknown;
  /** The column's formatted text for `value`. */
  text: string;
}

/** Read-only state supplied to caller-owned Table toolbar actions. */
export interface TableActionsContext {
  /** All filtered and sorted source rows, across pages; staged edits are supplied in changes. */
  rows: readonly TableRow[];
  /** Source rows on the current page, including virtualized rows outside the viewport. */
  pageRows: readonly TableRow[];
  /** Caller-controlled selection, or null when nothing is selected. */
  selectedRow: TableRowKey | null;
  /** Current immutable staged edits, for save or export actions. */
  changes: readonly TableChange[];
  /** Active column filters. */
  filters: TableFilters;
  /** Active sort, or null for source order. */
  sort: TableSort | null;
}

export interface TableColumn<Row extends TableRow = TableRow> {
  /** Stable property key used to read this column from a row. */
  key: keyof Row & string;
  /** Visible column heading. */
  header: string;
  /** Include this column in the sort cycle. */
  sortable?: boolean;
  /** Include a text field for this column in the filter row. */
  filterable?: boolean;
  /** Example query shown in this column's empty filter field. */
  filterPlaceholder?: string;
  /** Inline alignment. `end` also sets tabular numerals, for quantities that compare by column. */
  align?: TableColumnAlign;
  /** Allow this column to enter edit mode when the Table has editing capability. */
  editable?: boolean;
  /** Caller-owned comparison for values in this column. */
  compare?: (left: unknown, right: unknown, leftRow: Row, rightRow: Row) => number;
  /** Caller-owned filter predicate. The query has already been trimmed. */
  filter?: (value: unknown, query: string, row: Row) => boolean;
  /** Convert an editor's text back to the row's value type. */
  parse?: (input: string, previous: unknown, row: Row) => unknown;
  /** Format a value for read-only display and the initial editor text. */
  format?: (value: unknown, row: Row) => string;
}

export type TableFilters = Readonly<Record<string, string | undefined>>;

export interface PaginationOptions {
  /** One-based requested page. Out-of-range values are clamped. */
  page: number;
  pageSize: number;
}

export interface PaginatedRows<Row> {
  rows: readonly Row[];
  page: number;
  pageSize: number;
  pageCount: number;
  total: number;
  /** Zero-based inclusive index in the unpaginated collection. */
  start: number;
  /** Zero-based exclusive index in the unpaginated collection. */
  end: number;
}

export interface VirtualWindowOptions {
  scrollTop: number;
  viewportHeight: number;
  rowHeight: number;
  overscan?: number;
}

export interface VirtualWindow<Row> {
  items: readonly Row[];
  /** Zero-based inclusive row index. */
  start: number;
  /** Zero-based exclusive row index. */
  end: number;
  offsetTop: number;
  offsetBottom: number;
  totalHeight: number;
}

export interface TableChange<Value = unknown> {
  rowKey: TableRowKey;
  columnKey: string;
  previous: Value;
  value: Value;
}

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

function defaultCompare(left: unknown, right: unknown): number {
  if (Object.is(left, right)) return 0;
  if (left == null) return 1;
  if (right == null) return -1;
  if (typeof left === "number" && typeof right === "number") return left - right;
  if (left instanceof Date && right instanceof Date) return left.getTime() - right.getTime();
  return collator.compare(String(left), String(right));
}

/** Return a stably sorted copy. Missing columns or sort state leave the caller order intact. */
export function sortRows<Row extends TableRow>(
  rows: readonly Row[],
  columns: readonly TableColumn<Row>[],
  sort: TableSort | null | undefined,
): readonly Row[] {
  if (!sort) return [...rows];
  const column = columns.find((candidate) => candidate.key === sort.key);
  if (!column) return [...rows];
  const sign = sort.direction === "descending" ? -1 : 1;

  return rows
    .map((row, index) => ({ row, index }))
    .sort((left, right) => {
      const leftValue = left.row[column.key];
      const rightValue = right.row[column.key];
      if (leftValue == null || rightValue == null) {
        const compared = defaultCompare(leftValue, rightValue);
        return compared === 0 ? left.index - right.index : compared;
      }
      const compared = column.compare
        ? column.compare(leftValue, rightValue, left.row, right.row)
        : defaultCompare(leftValue, rightValue);
      return compared === 0 ? left.index - right.index : compared * sign;
    })
    .map(({ row }) => row);
}

/** Apply every non-empty column query with AND semantics without mutating rows. */
export function filterRows<Row extends TableRow>(
  rows: readonly Row[],
  columns: readonly TableColumn<Row>[],
  filters: TableFilters | null | undefined,
): readonly Row[] {
  if (!filters) return [...rows];
  const active = columns
    .map((column) => ({ column, query: filters[column.key]?.trim() ?? "" }))
    .filter(({ query }) => query.length > 0);
  if (active.length === 0) return [...rows];

  return rows.filter((row) =>
    active.every(({ column, query }) => {
      const value = row[column.key];
      if (column.filter) return column.filter(value, query, row);
      return String(value ?? "").toLocaleLowerCase().includes(query.toLocaleLowerCase());
    }),
  );
}

/** Slice a one-based page and report the clamped paging state. */
export function paginateRows<Row>(
  rows: readonly Row[],
  options: PaginationOptions,
): PaginatedRows<Row> {
  const pageSize = Math.max(1, Math.floor(options.pageSize) || 1);
  const pageCount = Math.max(1, Math.ceil(rows.length / pageSize));
  const page = Math.min(pageCount, Math.max(1, Math.floor(options.page) || 1));
  const start = (page - 1) * pageSize;
  const end = Math.min(rows.length, start + pageSize);
  return { rows: rows.slice(start, end), page, pageSize, pageCount, total: rows.length, start, end };
}

/** Compute an overscanned fixed-height window; the returned end is exclusive. */
export function virtualWindow<Row>(
  rows: readonly Row[],
  options: VirtualWindowOptions,
): VirtualWindow<Row> {
  const rowHeight = Math.max(1, options.rowHeight);
  const viewportHeight = Math.max(0, options.viewportHeight);
  const scrollTop = Math.max(0, options.scrollTop);
  const overscan = Math.max(0, Math.floor(options.overscan ?? 2));
  const visibleStart = Math.floor(scrollTop / rowHeight);
  const visibleEnd = Math.ceil((scrollTop + viewportHeight) / rowHeight);
  const start = Math.min(rows.length, Math.max(0, visibleStart - overscan));
  const end = Math.min(rows.length, Math.max(start, visibleEnd + overscan));

  return {
    items: rows.slice(start, end),
    start,
    end,
    offsetTop: start * rowHeight,
    offsetBottom: (rows.length - end) * rowHeight,
    totalHeight: rows.length * rowHeight,
  };
}

/**
 * Add or replace one cell change. Returning a value to its original state removes that cell
 * from the change-set; the original `previous` value survives repeated edits.
 */
export function stageChange<Value>(
  changes: readonly TableChange[],
  next: TableChange<Value>,
): readonly TableChange[] {
  const index = changes.findIndex(
    (change) => change.rowKey === next.rowKey && change.columnKey === next.columnKey,
  );
  const previous = index >= 0 ? changes[index]!.previous : next.previous;
  const withoutCell = changes.filter((_, candidate) => candidate !== index);
  if (Object.is(previous, next.value)) return withoutCell;
  const change: TableChange = { ...next, previous };
  return index < 0
    ? [...withoutCell, change]
    : [...withoutCell.slice(0, index), change, ...withoutCell.slice(index)];
}

/** Read a staged value when present, otherwise return the caller-owned row value. */
export function valueWithChanges(
  value: unknown,
  changes: readonly TableChange[],
  rowKey: TableRowKey,
  columnKey: string,
): unknown {
  const change = changes.find(
    (candidate) => candidate.rowKey === rowKey && candidate.columnKey === columnKey,
  );
  return change ? change.value : value;
}

/** Materialize a change-set into new objects, leaving untouched rows referentially intact. */
export function applyChanges<Row extends TableRow>(
  rows: readonly Row[],
  changes: readonly TableChange[],
  rowKey: (row: Row, index: number) => TableRowKey,
): readonly Row[] {
  if (changes.length === 0) return [...rows];
  return rows.map((row, index) => {
    const key = rowKey(row, index);
    const rowChanges = changes.filter((change) => change.rowKey === key);
    if (rowChanges.length === 0) return row;
    const copy = { ...row };
    for (const change of rowChanges) copy[change.columnKey as keyof Row] = change.value as Row[keyof Row];
    return copy;
  });
}
