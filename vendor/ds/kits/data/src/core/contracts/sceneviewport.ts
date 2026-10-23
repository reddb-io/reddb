import type { SceneBounds, SceneEngineFactory } from "../../scene-viewport";
import type { VisualizationSize } from "../../visualization";
import type { SceneAnnotation } from "../../scene-annotations";
export interface SceneViewportContract {
  /** Logical HTML labels; overlapping/off-scene labels are omitted, selected label wins collisions. */
  annotations?: readonly SceneAnnotation[];
  /** Selected label id, independent of camera following. */
  selected?: string | null;
  /** Requests annotation selection; omitted, labels are read-only. */
  onannotationselect?: (id: string) => void;
  /** Moving logical target; undefined hides follow controls, null disables them. */
  follow?: SceneBounds | null;
  /** Follow the supplied target. Manual navigation suspends following; reduced motion disables it. */
  following?: boolean;
  /** Reports follow controls, manual navigation, missing targets or reduced motion. */
  onfollowingchange?: (
    following: boolean,
    reason: "control" | "navigate" | "unavailable" | "reduced-motion",
  ) => void;
  /** Visible action label for starting or resuming following. */
  followLabel?: string;
  /** Visible action label while following. */
  pauseFollowLabel?: string;
  /** Explanation when motion preferences disable continuous following. */
  reducedMotionLabel?: string;
  /** Accessible name of the scene and keyboard-operable viewport. */
  label: string;
  /** Logical width of the static drawing. */
  contentWidth: number;
  /** Logical height of the static drawing. */
  contentHeight: number;
  /** Density-relative viewport height. */
  size?: VisualizationSize;
  /** Optional target bounds; clearing restores the overview. */
  focus?: SceneBounds | null;
  /** Show a decorative spatial grid beneath the drawing. */
  grid?: boolean;
  /** Optional caller-supplied renderer; no engine is bundled by default. */
  engine?: SceneEngineFactory;
  /** Caller-owned renderer input; the DS does not simulate or mutate it. */
  data?: unknown;
}
