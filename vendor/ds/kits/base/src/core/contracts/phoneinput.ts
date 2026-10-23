import type { CountryCode } from "libphonenumber-js/min";
import type { InputSize } from "../../input.variants";
/** Geographic country/territory supported by the pinned telephone metadata. */
export type PhoneCountry = CountryCode;
/** Framework-independent telephone field contract. */
export interface PhoneInputContract {
  /** Visible label associated with the national-number input. */
  label: string;
  /** International unmasked value: + followed by digits, or an empty string. Partial edits remain drafts until native validation succeeds. */
  value?: string;
  /** Initial uncontrolled international value, restored by form reset. */
  defaultValue?: string;
  /** Starting country for empty/national values. International values infer their country. Defaults to US. */
  defaultCountry?: PhoneCountry;
  /** Allowed geographic countries. Omitted means every country in the pinned metadata. */
  countries?: readonly PhoneCountry[];
  /** Locale used for country names and sorting; defaults to en. */
  locale?: string;
  /** Native form name. Exactly one unmasked international value is submitted. */
  name?: string;
  /** Optional external form id shared by the native controls. */
  form?: string;
  /** Native telephone input id; generated when omitted. */
  id?: string;
  /** Supporting text associated with the telephone input. */
  help?: string;
  /** Application error displayed separately from the label. */
  error?: string;
  /** Require a complete telephone number before the form can submit. */
  required?: boolean;
  /** Disable editing and omit the value from native form submission. */
  disabled?: boolean;
  /** Input by default; text renders a selectable formatted value without an editable control. */
  presentation?: "input" | "text";
  /** Optional caller-supplied provenance shown in text presentation. */
  sourceLabel?: string;
  /** Text used for an empty value in text presentation. Defaults to No phone number. */
  emptyLabel?: string;
  /** Placeholder for the national-number input. */
  placeholder?: string;
  /** Autofill token for the national-number control; defaults to tel-national. */
  autoComplete?: string;
  /** Prefix for the accessible country selector label, followed by the selected country name. Defaults to Country. */
  countryLabel?: string;
  /** Placeholder in the country search. */
  searchPlaceholder?: string;
  /** Message for a country search with no results. */
  emptyText?: string;
  /** Message when a required field is empty. */
  requiredMessage?: string;
  /** Message for an incomplete number or a country outside the allowed list. Metadata checks structure/length, not ownership. */
  invalidMessage?: string;
  /** Shared height for the country selector and national-number input. Defaults to md. */
  controlSize?: InputSize;
}
