import type { ListRowSize } from "../../primitives/list-row.variants";

/** Portable ListRow properties shared by native adapters. */
export interface ListRowContract {
/** The row's primary line. Ignored when a `children` snippet is given. */
    title?: string;
/** The secondary line under the title. */
    description?: string;
/** Renders the row as a link to here. */
    href?: string;
/** Row height and inset: `md` (the default) or `sm`. */
    size?: ListRowSize;
/** Mark the row as the one the list is currently about. */
    selected?: boolean;
}
