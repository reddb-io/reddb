import type { IsometricItem } from "../../isometric";
import type { VisualizationSize } from "../../visualization";
export interface IsometricSceneContract {
  /** Visible and accessible scene name. */
  label: string;
  /** Nonintersecting solid boxes with stable ids and logical coordinates. */
  items: readonly IsometricItem[];
  /** Caller-selected entity id. */
  selected?: string | null;
  /** Focus the viewport on this entity; null restores overview. */
  focused?: string | null;
  /** Limit the drawing to this caller-owned layer name. */
  layer?: string | null;
  /** Whether boxes expose keyboard selection. */
  interactive?: boolean;
  /** Density-relative viewport height. */
  size?: VisualizationSize;
  /** Reports selection to application-owned inspectors and other views. */
  onselectedchange?: (id: string) => void;
}
