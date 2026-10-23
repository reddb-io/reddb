import type { TreeNode } from "../../tree";
/** Portable TreeView properties shared by native adapters. */
export interface TreeViewContract {
    /** Required visible name for the tree. */
    label: string;
    /**
         * The tree to show: root nodes, each with a stable `id`, a visible `label` and its `children`.
         * Children render only while their parent's id is expanded.
         */
    nodes: readonly TreeNode[];
    /** Optional caller-controlled expanded node ids. */
    expanded?: readonly string[];
    /** Optional caller-controlled selected node id. */
    selected?: string | null;
    /** Rename capability, offered when `onrename` is given. A predicate can limit which nodes expose it. */
    renamable?: boolean | ((node: TreeNode) => boolean);
    /** Drag-and-drop reorder capability, offered when `onreorder` is given. A predicate can limit draggable nodes. */
    reorderable?: boolean | ((node: TreeNode) => boolean);
}
