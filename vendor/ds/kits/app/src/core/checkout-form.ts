/** One caller-owned checkout field rendered through canonical Field and Input. */
export interface CheckoutField<Type = unknown, Complete = unknown, InputMode = unknown> {
  name: string;
  label: string;
  type?: Type;
  value?: string | number;
  autocomplete?: Complete;
  inputmode?: InputMode;
  placeholder?: string;
  help?: string;
  error?: string;
  required?: boolean;
  disabled?: boolean;
}

/** A visibly named caller-owned group of checkout fields. */
export interface CheckoutSection<Type = unknown, Complete = unknown, InputMode = unknown> {
  legend: string;
  fields: readonly CheckoutField<Type, Complete, InputMode>[];
  disabled?: boolean;
}

