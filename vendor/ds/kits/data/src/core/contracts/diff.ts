/** Portable Diff properties shared by native adapters. */
export interface DiffContract {
    /**
         * The earlier value. In `fields` mode any value is flattened and compared field by field; in
         * `lines` mode it is turned into text and split into lines (`null` and `undefined` become empty
         * text).
         */
    before: unknown;
    /**
         * The later value, compared against `before`. Entries present only here are reported as added;
         * the shape expected is the same as `before`.
         */
    after: unknown;
    /** Fields recursively compare values; lines compares string lines with LCS alignment. */
    mode?: "fields" | "lines";
    /** Accessible name for the diff region. Defaults to `Changes`. */
    label?: string;
    /**
         * Whether entries identical in both values are listed. When `false`, only added, removed and
         * changed entries show; the summary still counts the unchanged ones. Defaults to `true`.
         */
    showUnchanged?: boolean;
}
