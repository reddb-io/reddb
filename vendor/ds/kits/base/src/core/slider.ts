/** Exact decimal parts keep range step ties independent of floating-point noise. */
function decimal(value: number) {
  const [mantissa, exponent = "0"] = String(value).split("e");
  const fraction = mantissa!.split(".")[1]?.length ?? 0;
  return { coefficient: BigInt(mantissa!.replace(".", "")), exponent: Number(exponent) - fraction };
}
/** Match native range sanitization so the SSR readout agrees with the browser. */
export function rangeValue(value: number, min: number, max: number, step: number | "any"): number {
  const lower = Number.isFinite(min) ? min : 0;
  const upper = Math.max(lower, Number.isFinite(max) ? max : 100);
  const next = Math.min(upper, Math.max(lower, Number.isFinite(value) ? value : lower / 2 + upper / 2));
  if (step === "any") return next;
  const increment = Number.isFinite(step) && step > 0 ? step : 1;
  const parts = [next, lower, upper, increment].map(decimal);
  const exponent = Math.min(...parts.map(part => part.exponent));
  const [chosen, first, last, stride] = parts.map(part => part.coefficient * 10n ** BigInt(part.exponent - exponent)) as [bigint, bigint, bigint, bigint];
  const offset = chosen - first;
  let index = offset / stride;
  if ((offset % stride) * 2n >= stride) index++;
  const maximum = (last - first) / stride;
  if (index > maximum) index = maximum;
  return Number(`${first + index * stride}e${exponent}`);
}
