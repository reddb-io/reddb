import { getDayOfWeek, getLocalTimeZone, parseDate, startOfMonth, today, type DateValue } from "@internationalized/date";
/** A range can be incomplete while its second bound is being chosen. */
export interface CalendarRange { start: DateValue | undefined; end: DateValue | undefined }
/** Calendar semantics independent of either headless rendering implementation. */
export interface CalendarContract {
  /** Base accessible name; the visible month and year are appended. */
  label: string;
  /** Month shown when no value is selected; moves with keyboard or month navigation. */
  placeholder?: DateValue;
  /** Earliest selectable date. */
  minValue?: DateValue;
  /** Latest selectable date. */
  maxValue?: DateValue;
  /** Prevent focus and selection of every date and navigation control. */
  disabled?: boolean;
  /** Permit navigation and focus without changing selection. */
  readonly?: boolean;
  /** Prevent clearing a selected date by selecting it again. */
  preventDeselect?: boolean;
  /** Mark individual dates disabled and exclude them from keyboard focus. */
  isDateDisabled?: (date: DateValue) => boolean;
  /** Mark dates unavailable for selection. */
  isDateUnavailable?: (date: DateValue) => boolean;
  /** Locale for calendar labels, date announcements and right-to-left direction. */
  locale?: string;
  /** First weekday, Sunday=0 through Saturday=6. */
  weekStartsOn?: 0 | 1 | 2 | 3 | 4 | 5 | 6;
  /** Width of weekday names; defaults to short. */
  weekdayFormat?: Intl.DateTimeFormatOptions["weekday"];
  /** Render six weeks per month to keep the height stable. Defaults to true. */
  fixedWeeks?: boolean;
  /** Number of simultaneous months; defaults to one. */
  numberOfMonths?: number;
  /** Move by the number of visible months rather than one month. */
  pagedNavigation?: boolean;
  /** Disable dates drawn outside each grid's named month. */
  disableDaysOutsideMonth?: boolean;
  /** Formatting of the visible month name. */
  monthFormat?: Intl.DateTimeFormatOptions["month"] | ((month: number) => string);
  /** Formatting of the visible year. */
  yearFormat?: Intl.DateTimeFormatOptions["year"] | ((year: number) => string);
  /** Extra classes merged onto both month navigation controls. */
  navigationClass?: string;
  /** Extra classes merged onto every day control. */
  dayClass?: string;
}
export function parseCalendarDate(value?: string): DateValue | undefined {
  try { return value ? parseDate(value) : undefined; } catch { return undefined; }
}
export function currentCalendarDate() { return today(getLocalTimeZone()); }
export function calendarMonths(date: DateValue, count: number, weekStart: number, fixed: boolean, locale: string) {
  return Array.from({ length: Math.max(1, Math.trunc(count) || 1) }, (_, index) => {
    const month = startOfMonth(date.add({ months: index }));
    const first = month.subtract({ days: (getDayOfWeek(month, locale, "sun") - weekStart + 7) % 7 });
    const length = fixed ? 42 : Math.ceil(((getDayOfWeek(month, locale, "sun") - weekStart + 7) % 7 + month.calendar.getDaysInMonth(month)) / 7) * 7;
    return { month, days: Array.from({ length }, (_, index) => first.add({ days: index })) };
  });
}
export function calendarDisabled(date: DateValue, options: Pick<CalendarContract, "disabled" | "minValue" | "maxValue" | "isDateDisabled">) {
  return !!(options.disabled || (options.minValue && date.compare(options.minValue) < 0) || (options.maxValue && date.compare(options.maxValue) > 0) || options.isDateDisabled?.(date));
}
export function calendarRangeValid(start: DateValue, end: DateValue, options: CalendarContract & { minDays?: number; maxDays?: number; excludeDisabled?: boolean }) {
  const length = end.calendar.toJulianDay(end) - start.calendar.toJulianDay(start) + 1;
  if (length < (options.minDays ?? 1) || length > (options.maxDays ?? Infinity)) return false;
  if (options.excludeDisabled) for (let day = start; day.compare(end) <= 0; day = day.add({ days: 1 })) {
    if (calendarDisabled(day, options) || options.isDateUnavailable?.(day)) return false;
  }
  return true;
}
/** Arrow/day, week-edge and month/year moves; disabled dates are skipped within the bounds. */
export function calendarKeyTarget(date: DateValue, key: string, options: CalendarContract, shift = false, rtl = false): DateValue | undefined {
  const weekStart = options.weekStartsOn ?? 0, weekday = getDayOfWeek(date, options.locale ?? "en", "sun");
  let next = key === "ArrowLeft" ? date.add({ days: rtl ? 1 : -1 }) : key === "ArrowRight" ? date.add({ days: rtl ? -1 : 1 }) : key === "ArrowUp" ? date.subtract({ days: 7 }) : key === "ArrowDown" ? date.add({ days: 7 }) : key === "Home" ? date.subtract({ days: (weekday - weekStart + 7) % 7 }) : key === "End" ? date.add({ days: 6 - (weekday - weekStart + 7) % 7 }) : key === "PageUp" ? date.subtract(shift ? { years: 1 } : { months: 1 }) : key === "PageDown" ? date.add(shift ? { years: 1 } : { months: 1 }) : undefined;
  if (!next || options.disabled) return undefined;
  if (options.minValue && next.compare(options.minValue) < 0) next = options.minValue;
  if (options.maxValue && next.compare(options.maxValue) > 0) next = options.maxValue;
  const step = next.compare(date) >= 0 ? 1 : -1;
  for (let attempt = 0; attempt < 366; attempt++) {
    if (!calendarDisabled(next, options)) return next;
    next = next.add({ days: step });
    if ((options.minValue && next.compare(options.minValue) < 0) || (options.maxValue && next.compare(options.maxValue) > 0)) return undefined;
  }
  return undefined;
}

/** Resolve text direction for language and script locales across modern Intl implementations. */
export function calendarDirection(locale: string): "ltr" | "rtl" {
  const info = new Intl.Locale(locale) as Intl.Locale & { textInfo?: { direction: string }; getTextInfo?: () => { direction: string } };
  return (info.getTextInfo?.().direction ?? info.textInfo?.direction ?? (/^(ar|fa|he|ur)(-|$)/.test(locale) ? "rtl" : "ltr")) === "rtl" ? "rtl" : "ltr";
}

/** Keep a DateValue's calendar identity when a renderer derives its calendar from locale. */
export function calendarLocale(locale: string, date: DateValue | undefined): string {
  return date ? new Intl.Locale(locale, { calendar: date.calendar.identifier }).toString() : locale;
}
