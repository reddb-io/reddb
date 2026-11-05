export type TableCellValue = string | number | null | undefined;

export interface TableColumn {
  /** Stable key used to read this column from each row. */
  key: string;
  /** Visible column heading. */
  header: string;
}

export type TableRow = Readonly<Record<string, TableCellValue>>;

export interface TableCellContext {
  /** Current canonical column. */
  column: TableColumn;
  /** Current caller-owned row. */
  row: TableRow;
  /** Cell value read from the column key. */
  value: TableCellValue;
  /** Zero-based position in the current rows. */
  rowIndex: number;
}
