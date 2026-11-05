import type { WorkflowData } from "../../workflow";
import type { VisualizationSize } from "../../visualization";
export interface WorkflowDiagramContract {
  /** Visible and accessible workflow name. */
  label: string;
  /** Tasks, named ports and directed channels with stable ids. */
  data: WorkflowData;
  /** Rank direction; coordinates remain owned by shared portable geometry. */
  direction?: "right" | "down";
  /** Caller-controlled selected node or edge id. */
  selected?: string | null;
  /** Focus the viewport on this node; null restores overview. */
  focused?: string | null;
  /** Enable native keyboard and pointer selection. */
  interactive?: boolean;
  /** Opt-in activity markers; suspended outside the viewport, in hidden tabs and under reduced motion. */
  playing?: boolean;
  /** Density-relative viewport height. */
  size?: VisualizationSize;
  /** Reports requested selection for caller-owned inspection. */
  onselectedchange?: (id: string) => void;
}
