import type { TableChange, TableColumn, TableFilters, TableRow, TableRowKey, TableSort, VirtualWindowOptions } from "../../table";
interface TableVirtualization extends Omit<VirtualWindowOptions, "scrollTop"> {
}
/** Portable Table properties shared by native adapters. */
export interface TableContract {
    /** Required visible name for the grid. */
    caption: string;
    /**
         * Column definitions in display order: each names the row property it reads (`key`) and its
         * `header`, and can opt into sorting, filtering, editing, alignment or custom formatting.
         */
    columns: readonly TableColumn[];
    /**
         * The row objects to show, read by column `key`. The Table never mutates them: filtering,
         * sorting, paging and staged edits all work on copies.
         */
    rows: readonly TableRow[];
    /** Stable row property or resolver. Falls back to the row's current data index. */
    rowKey?: string | ((row: TableRow, index: number) => TableRowKey);
    /** Editing capability. Columns must also opt in with `editable: true`. */
    editable?: boolean | ((row: TableRow, column: TableColumn) => boolean);
    /** Optional controlled change-set. Without it, the component keeps a local staged set. */
    changes?: readonly TableChange[];
    /**
         * Active sort as `{ key, direction }`, or `null` for the rows' own order. Passing it (even
         * `null`) makes sorting caller-controlled; omitted, the Table keeps its own sort.
         */
    sort?: TableSort | null;
    /**
         * Filter queries keyed by column `key`; only `filterable` columns show a field. Passing it
         * makes filtering caller-controlled; omitted, the Table keeps its own.
         */
    filters?: TableFilters;
    /**
         * One-based current page, clamped to the available pages. Passing it makes paging
         * caller-controlled; omitted, the Table starts on page `1` and tracks it itself.
         */
    page?: number;
    /**
         * Rows per page; the pager appears only when the rows span more than one page. Defaults to
         * `25`.
         */
    pageSize?: number;
    /** Fixed-height row virtualization. Windowing is applied inside the current page. */
    virtualize?: TableVirtualization | false;
    /** Keep the caption for assistive technology while a surrounding heading names the grid visibly. */
    captionHidden?: boolean;
    /** Offer the column visibility picker. Defaults to true. */
    columnVisibility?: boolean;
    /** Plain text shown when no row survives filtering and no \`empty\` snippet is given. */
    emptyMessage?: string;
    /** Controlled selected row key. Rows become selectable when `onselect` is given. */
    selectedRow?: TableRowKey | null;
    /**
         * Extra classes, merged onto the outermost wrapper that holds the filter and column tools, the
         * scroll region and the pager.
         */
    containerClass?: string;
}
