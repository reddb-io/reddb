/** Portable JsonTree properties shared by native adapters. */
export interface JsonTreeContract {
    /** JSON-compatible caller-owned value to inspect. */
    value: unknown;
    /** Accessible name for the tree. */
    label?: string;
    /**
         * Paths of the nodes held open, in the same dotted form as `selected`. Passing it makes
         * expansion caller-controlled; omitted, the tree keeps its own open state, starting collapsed.
         */
    expanded?: readonly string[];
    /** The selected node's dotted path. */
    selected?: string | null;
}
