/** One aligned comparison row. The consumer owns diff computation. */
  export interface DiffRow {
    /** The line before the change; omit it for a pure addition. */
    before?: string;
    /** The line after the change; omit it for a pure removal. */
    after?: string;
  }
