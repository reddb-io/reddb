import type { ScheduleOverlays } from "../schedule-overlays";
import type { TimeInterval } from "../../intervals";
import type { VisualizationSize } from "../../visualization";
/** Portable IntervalPlot properties shared by native adapters. */
export interface IntervalPlotContract extends ScheduleOverlays {
    data: readonly TimeInterval[];
    kind: "Gantt" | "TraceWaterfall";
    label: string;
    description?: string;
    width?: number;
    height?: number;
    size?: VisualizationSize;
    selected?: string;
    interactive?: boolean;
    detailHref?: string;
    timeDomain?: readonly [
        number,
        number
    ];
    formatTime?: (value: number) => string;
    formatDuration?: (value: number) => string;
    collapsed?: readonly string[];
}
