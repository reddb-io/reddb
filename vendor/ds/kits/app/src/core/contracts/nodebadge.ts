import type { NodeStatus } from "../../primitives/node-badge.variants";

/** Portable NodeBadge properties shared by native adapters. */
export interface NodeBadgeContract {
/** The node's name, as the cluster knows it. */
    name: string;
/** Reachability. Defaults to `unknown` — no reading is not a good reading. */
    status?: NodeStatus;
/** Draw the status word beside the name. Defaults to true. */
    showStatus?: boolean;
}
