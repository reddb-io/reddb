import type { DeprecatedKpiCardTone, KpiCardTone } from "../../kpi-card.variants";
import type { KpiPolarity } from "../kpi-card";
/** Portable KpiCard properties shared by native adapters. */
export interface KpiCardContract {
    /** Name of the metric, shown above the value and used as the card's accessible name. */
    label: string;
    /**
         * The headline figure, shown as given: format it before passing it (a number is rendered as
         * is).
         */
    value: string | number;
    /**
         * Change against the comparison period, as a number (`4.2` shows `+4.2%`). Its sign sets the
         * direction and, with `polarity`, the colour; omitted or `null`, no delta is shown.
         */
    delta?: number | null;
    /** Text appended to the delta in its visible figure, such as a unit. Defaults to `%`. */
    deltaSuffix?: string;
    /**
         * Visible comparison context, also read after the delta. Defaults to
         * `versus previous period`.
         */
    deltaLabel?: string;
    /** Optional visible context below the value, such as the measured period or a short explanation. The comparison label appears alongside the delta when a delta is supplied. */
    description?: string;
    /**
         * Which direction of change is good news. Delta colour follows meaning, not
         * sign: a latency drop is an improvement under `lower-is-better`, and a
         * `neutral` metric never claims a direction is good or bad.
         */
    polarity?: KpiPolarity;
    /**
         * What the metric means: the shared tone vocabulary (ADR 0026), each on its
         * Feedback Role's surface with its paired readable foreground. Defaults to
         * `neutral`. `brand` is deprecated for one release.
         */
    tone?: KpiCardTone | DeprecatedKpiCardTone;
}
