import type { PanZoomView } from "../../pan-zoom";
import type { VisualizationSize } from "../../visualization";
/** Portable PanZoomViewport properties shared by native adapters. */
export interface PanZoomViewportContract {
  /** Accessible name of the keyboard-operable drawing viewport. */
  label: string;
  /** Logical width of the caller's drawing, independent of Density. */
  contentWidth: number;
  /** Logical height of the caller's drawing, independent of Density. */
  contentHeight: number;
  /** Current translation and scale. Omitted, fits the drawing on first mount. */
  view?: PanZoomView;
  /** Reports manual pan, zoom, fit or focus reveal; initial fitting is excluded. */
  onnavigate?: () => void;
  /** Smallest allowed scale, a positive factor. */
  minScale?: number;
  /** Largest allowed scale, at least minScale. */
  maxScale?: number;
  /** Height in the visualization family's Density-relative scale. */
  size?: VisualizationSize;
  /** Optional viewport width/height ratio; size then caps its Density-relative height. */
  aspectRatio?: number;
  /** Show zoom and fit controls; a supplied controls snippet replaces the defaults. */
  showControls?: boolean;
  /** Visible and accessible zoom-in control label. */
  zoomInLabel?: string;
  /** Visible and accessible zoom-out control label. */
  zoomOutLabel?: string;
  /** Visible fit-to-content control label. */
  fitLabel?: string;
  /** Instructions announced when the viewport receives focus. */
  instructions?: string;
}
