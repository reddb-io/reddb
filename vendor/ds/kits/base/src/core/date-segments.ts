/** Parse a complete possible Gregorian date without timezone conversion. */
export function possibleDate(candidate: string): RegExpMatchArray | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(candidate);
  if (!match) return null;
  const year = Number(match[1]), month = Number(match[2]), day = Number(match[3]);
  if (year === 0 || month < 1 || month > 12) return null;
  const leap = year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0);
  const days = [31, leap ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
  return day >= 1 && day <= days[month - 1]! ? match : null;
}
export function possibleTime(candidate: string): RegExpMatchArray | null { return /^([01]\d|2[0-3]):([0-5]\d)$/.exec(candidate); }
export type DateSegment = "year" | "month" | "day" | "hour" | "minute";
export function segmentDigits(value: string, segment: DateSegment): string {
  const length = segment === "year" ? 4 : 2, digits = value.replace(/\D/g, "").slice(0, length), number = Number(digits);
  const limits: Record<DateSegment, readonly [number, number]> = { year: [1, 9999], month: [1, 12], day: [1, 31], hour: [0, 23], minute: [0, 59] };
  const [min, max] = limits[segment];
  return digits.length === length && (number < min || number > max) ? "" : digits;
}
