/** The numeric native input types report numbers; an empty number reports null. */
export type InputValue<Type extends string> = string extends Type ? string | number | null : Type extends "number" | "range" ? number | null : string;

/** Read a platform input without depending on a rendering framework or globals. */
export function inputValue<Type extends string>(input: { type: Type; value: string; valueAsNumber: number }): InputValue<Type> {
  return (input.type === "number" || input.type === "range"
    ? input.value === "" ? null : input.valueAsNumber
    : input.value) as InputValue<Type>;
}
