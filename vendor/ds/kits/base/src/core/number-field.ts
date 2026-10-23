/** Native number constraints shared by both adapters; arithmetic stays in stepUp/stepDown. */
export function numberConstraints(min: number | undefined, max: number | undefined, step: number) {
  if ((min !== undefined && !Number.isFinite(min)) || (max !== undefined && !Number.isFinite(max)) ||
    !Number.isFinite(step) || step <= 0 || (min !== undefined && max !== undefined && max < min)) {
    throw new Error("NumberField: use finite bounds, max >= min, and a positive finite step.");
  }
  return { min, max, step };
}
