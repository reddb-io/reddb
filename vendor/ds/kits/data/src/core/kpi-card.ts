export type KpiDeltaDirection = "increase" | "decrease" | "unchanged";
/** Which direction of change is good news for this metric. */
export type KpiPolarity = "higher-is-better" | "lower-is-better" | "neutral";
/** What a delta means, independent of its sign. */
export type KpiDeltaSentiment = "improvement" | "regression" | "neutral";
export function kpiDeltaDirection(delta: number): KpiDeltaDirection {
    if (delta > 0)
        return "increase";
    if (delta < 0)
        return "decrease";
    return "unchanged";
}
export function kpiDeltaSentiment(delta: number, polarity: KpiPolarity = "higher-is-better"): KpiDeltaSentiment {
    const direction = kpiDeltaDirection(delta);
    if (direction === "unchanged" || polarity === "neutral")
        return "neutral";
    const better = polarity === "higher-is-better" ? "increase" : "decrease";
    return direction === better ? "improvement" : "regression";
}
export { DEPRECATED_KPI_CARD_TONES, KPI_CARD_TONES, type DeprecatedKpiCardTone, type KpiCardTone, } from "../kpi-card.variants";
export function formatKpiDelta(delta: number, suffix = "%"): string {
    return `${delta > 0 ? "+" : ""}${delta}${suffix}`;
}
