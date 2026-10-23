import { AsYouType, getCountries, getCountryCallingCode, parseIncompletePhoneNumber } from "libphonenumber-js/min";
import type { PhoneCountry } from "./contracts/phoneinput";
import type { SearchSelectOption } from "../search-select.variants";
export type { PhoneCountry } from "./contracts/phoneinput";
export interface PhoneState {
  country: PhoneCountry;
  display: string;
  value: string;
  international: string;
  possible: boolean;
}

/** An empty allowlist is a configuration error, never an implicit unrestricted selector. */
export function phoneCountries(countries?: readonly PhoneCountry[]): readonly PhoneCountry[] {
  const supported = new Set(getCountries()), result = [...new Set(countries ?? getCountries())];
  if (!result.length || result.some(country => !supported.has(country))) throw new RangeError("PhoneInput requires supported country codes and a non-empty country list.");
  return result;
}
/** Unicode flag is decorative; the selector always also names the country and calling code. */
export function phoneFlag(country: PhoneCountry): string {
  return [...country].map(letter => String.fromCodePoint(127397 + letter.charCodeAt(0))).join("");
}
export function phoneCountryName(country: PhoneCountry, locale = "en"): string {
  return new Intl.DisplayNames([locale], { type: "region" }).of(country) ?? country;
}
const optionCache = new Map<string, readonly SearchSelectOption[]>();
/** Search accepts localized/English names, ISO code and calling code. No network or location inference.
 * Before hydration, use ISO ordering/names: host ICU versions can disagree on translations and sort order. */
export function phoneCountryOptions(countries: readonly PhoneCountry[], locale = "en", localized = true): readonly SearchSelectOption[] {
  const key = `${localized ? locale : "iso"}:${countries.join(",")}`;
  const cached = optionCache.get(key); if (cached) return cached;
  const options = countries.map(country => ({ value: country, label: `${phoneFlag(country)} +${getCountryCallingCode(country)}`,
    description: localized ? phoneCountryName(country, locale) : country, keywords: [country, ...(localized ? [phoneCountryName(country, "en")] : []), getCountryCallingCode(country)] }));
  options.sort((a, b) => localized ? a.description.localeCompare(b.description, locale) : a.value < b.value ? -1 : 1);
  optionCache.set(key, options); return options;
}
/** Preserve the actual international prefix when pasting; never silently prepend the old country's code. */
export function phoneState(text: string, fallback: PhoneCountry): PhoneState {
  const clean = parseIncompletePhoneNumber(text), international = clean.startsWith("+");
  if (!clean || clean === "+") return { country: fallback, display: clean, value: "", international: clean, possible: false };
  const formatter = new AsYouType(fallback), formatted = formatter.input(clean), number = formatter.getNumber();
  const country = formatter.getCountry() ?? fallback;
  const value = String(formatter.getNumberValue() ?? (international ? clean : `+${getCountryCallingCode(fallback)}${clean}`));
  const display = international && formatter.getCountry() && number ? number.formatNational() : formatted;
  const hasExtension = /(?:ext\.?|extension|[#x])\s*\d/i.test(text);
  return { country, display, value, international: number && formatter.getCountry() ? number.formatInternational() : new AsYouType().input(value),
    possible: Boolean(number?.isPossible() && formatter.getCountry() && value.length <= 16 && !hasExtension) };
}
export function phoneProblem(state: PhoneState, countries: readonly PhoneCountry[], required: boolean, requiredMessage: string, invalidMessage: string): string {
  return !state.value && !state.display ? (required ? requiredMessage : "") : state.possible && countries.includes(state.country) ? "" : invalidMessage;
}
/** Count decimal digits rather than punctuation when preserving a selection through a mask. */
export function phoneDigitCount(value: string): number { return [...value.matchAll(/\p{Decimal_Number}/gu)].length; }
export function phoneCaret(display: string, digits: number): number {
  if (!digits) return 0;
  for (let i = 0, count = 0; i < display.length; i++) if (/\p{Decimal_Number}/u.test(display[i]!) && ++count >= digits) return i + 1;
  return display.length;
}
/** Delete the adjacent digit when Backspace/Delete would otherwise merely remove an auto-inserted separator. */
export function phoneSeparatorDeletion(display: string, type: string, start: number, end: number): { text: string; digits: number } | undefined {
  if (start !== end || !["deleteContentBackward", "deleteContentForward"].includes(type)) return;
  const backward = type === "deleteContentBackward", character = display[backward ? start - 1 : start];
  if (!character || /\p{Decimal_Number}/u.test(character)) return;
  const digits = parseIncompletePhoneNumber(display).replace(/\D/g, ""), before = phoneDigitCount(display.slice(0, start));
  const index = backward ? before - 1 : before;
  if (index < 0 || index >= digits.length) return;
  return { text: digits.slice(0, index) + digits.slice(index + 1), digits: backward ? before - 1 : before };
}
/** Native editing mechanics shared by both adapters: caret, paste, IME and separator deletion. */
export function bindPhoneEditing(element: HTMLInputElement, initial: PhoneState, changed: (next: PhoneState, previous: PhoneState) => void) {
  let current = initial, composing = false;
  const blocked = () => element.matches(":disabled");
  function apply(text = element.value, digitPosition = phoneDigitCount(text.slice(0, element.selectionStart ?? text.length))) {
    if (blocked()) return;
    const next = phoneState(text, current.country), previous = current; current = next;
    element.value = next.display;
    const caret = phoneCaret(next.display, digitPosition);
    element.setSelectionRange(caret, caret);
    if (next.value !== previous.value || next.country !== previous.country || next.display !== previous.display) changed(next, previous);
  }
  function input(event: Event) { if (!composing && !(event as InputEvent).isComposing) apply(); }
  function before(event: InputEvent) {
    if (composing || event.isComposing || blocked()) return;
    const deletion = phoneSeparatorDeletion(current.display, event.inputType, element.selectionStart ?? 0, element.selectionEnd ?? 0);
    if (deletion) { event.preventDefault(); apply(deletion.text, deletion.digits); }
  }
  function compositionStart() { composing = true; }
  function compositionEnd() { composing = false; apply(); }
  element.addEventListener("input", input); element.addEventListener("beforeinput", before);
  element.addEventListener("compositionstart", compositionStart); element.addEventListener("compositionend", compositionEnd);
  return {
    sync(next: PhoneState) { current = next; if (!composing && element.value !== next.display) element.value = next.display; },
    destroy() { element.removeEventListener("input", input); element.removeEventListener("beforeinput", before); element.removeEventListener("compositionstart", compositionStart); element.removeEventListener("compositionend", compositionEnd); },
  };
}

/** Keep the selected country accurate even when an international paste is outside the allowlist. */
export function phoneOptions(countries: readonly PhoneCountry[], country: PhoneCountry, locale: string, localized = true) {
  const options = phoneCountryOptions(countries, locale, localized);
  return countries.includes(country) ? options : [...options, { ...phoneCountryOptions([country], locale, localized)[0]!, disabled: true }];
}
/** Focus only the first invalid control when native form validation reports several errors. */
export function phoneInvalid(element: HTMLInputElement) {
  const first = element.form ? [...element.form.elements].find(control => "willValidate" in control && (control as HTMLInputElement).willValidate && !(control as HTMLInputElement).validity.valid) : element;
  if (first === element) element.focus();
}
