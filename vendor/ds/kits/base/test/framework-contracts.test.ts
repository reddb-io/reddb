import { mount, unmount, flushSync } from "svelte";
import { act, createElement as h, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it } from "vitest";
import SvelteField from "./fixtures/FrameworkField.svelte";
import SvelteTextarea from "./fixtures/FrameworkTextarea.svelte";
import { Textarea as ReactTextarea } from "../react/src/Textarea";
import SvelteInput from "./fixtures/FrameworkInput.svelte";
import { Field as ReactField } from "../react/src/Field";
import { Input as ReactInput } from "../react/src/Input";
import type { FieldContractProps } from "../src/core/field";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
let cleanup: (() => Promise<unknown>) | undefined;
afterEach(async () => { await cleanup?.(); cleanup = undefined; document.body.replaceChildren(); });

async function renderField(framework: "svelte" | "react", props: FieldContractProps) {
  const target = document.createElement("div"); document.body.append(target);
  if (framework === "svelte") {
    const component = mount(SvelteField, { target, props }); flushSync();
    cleanup = () => unmount(component);
    return { target, clearError: async () => flushSync(() => component.clearError()) };
  }
  const root = createRoot(target);
  const render = (next: FieldContractProps) => h(ReactField, { ...next, children: control => h(ReactInput<"text">, { ...control, name: "name" }) });
  await act(() => root.render(render(props)));
  cleanup = async () => { await act(() => root.unmount()); };
  return { target, clearError: async () => { await act(() => root.render(render({ ...props, error: undefined }))); } };
}

type InputType = "text" | "number" | "range";
async function renderInput(framework: "svelte" | "react", type: InputType, initialValue: string | number | null, changed: (value: unknown) => void) {
  const target = document.createElement("div"); document.body.append(target);
  if (framework === "svelte") {
    const component = mount(SvelteInput, { target, props: { type, initialValue, onValueChange: changed } }); flushSync();
    cleanup = () => unmount(component);
    return { target, update: async (value: string | number | null) => flushSync(() => component.setValue(value)) };
  }
  let update: (value: string | number | null) => void = () => {};
  function Control() {
    const [value, setValue] = useState(initialValue);
    update = setValue;
    return h(ReactInput<InputType>, { type, value, min: 0, max: 10, step: "any", "aria-label": "Value", name: "value", onValueChange: next => { changed(next); setValue(next); } });
  }
  const root: Root = createRoot(target);
  await act(() => root.render(h(Control)));
  cleanup = async () => { await act(() => root.unmount()); };
  return { target, update: async (value: string | number | null) => { await act(() => update(value)); } };
}

for (const framework of ["svelte", "react"] as const) describe(`${framework}: shared component contracts`, () => {
  for (const layout of ["stacked", "inline"] as const) for (const required of [false, true]) for (const messages of [
    {}, { help: "Use a recognizable name" }, { error: "Choose a name" }, { help: "Use a recognizable name", error: "Choose a name" },
  ]) it(`Field associates its native control (${layout}, required=${required}, ${JSON.stringify(messages)})`, async () => {
    const { target, clearError } = await renderField(framework, { label: "Name", layout, required, ...messages });
    const input = target.querySelector("input")!;
    expect(input.labels?.[0]?.htmlFor).toBe(input.id);
    expect(input.labels?.[0]?.textContent).toContain("Name");
    expect(input.required).toBe(required);
    expect(input.validity.valueMissing).toBe(required);
    expect(target.querySelector("[data-field-layout]")?.getAttribute("data-field-layout")).toBe(layout);
    const descriptions = input.getAttribute("aria-describedby")?.split(" ").map(id => document.getElementById(id)?.textContent) ?? [];
    expect(descriptions).toEqual([messages.help, messages.error].filter(Boolean));
    expect(input.getAttribute("aria-invalid")).toBe(messages.error ? "true" : null);
    expect(target.querySelector('[role="alert"]')?.textContent).toBe(messages.error);
    const original = input.id;
    await clearError();
    expect(input.id).toBe(original);
    expect(input.getAttribute("aria-invalid")).toBe(null);
    expect(input.getAttribute("aria-errormessage")).toBe(null);
    expect(target.querySelector('[role="alert"]')).toBe(null);
  });

  for (const scenario of [
    { type: "text", initial: "before", typed: "after", expected: "after" },
    { type: "number", initial: 0.3, typed: "0.5", expected: 0.5 },
    { type: "number", initial: 3, typed: "", expected: null },
    { type: "number", initial: 3, typed: "0", expected: 0 },
    { type: "range", initial: 2, typed: "3.5", expected: 3.5 },
  ] as const) it(`Input preserves native value meaning: ${scenario.type} ${JSON.stringify(scenario.typed)}`, async () => {
    const values: unknown[] = [];
    const { target } = await renderInput(framework, scenario.type, scenario.initial, value => values.push(value));
    const input = target.querySelector("input")!;
    await act(() => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, scenario.typed);
      input.dispatchEvent(new Event("input", { bubbles: true }));
      flushSync();
    });
    expect(values).toEqual([scenario.expected]);
    expect(input.value).toBe(scenario.typed);
  });

  it("Textarea preserves multiline payloads, form values and controlled clearing", async () => {
    const values: string[] = [];
    const target = document.createElement("form"); document.body.append(target);
    let update: (value: string) => Promise<unknown>;
    if (framework === "svelte") {
      const component = mount(SvelteTextarea, { target, props: { initialValue: "before", onValueChange: (value: string) => values.push(value) } }); flushSync();
      cleanup = () => unmount(component);
      update = async value => flushSync(() => component.setValue(value));
    } else {
      const root = createRoot(target);
      const render = (value: string) => h(ReactTextarea, { value, name: "notes", "aria-label": "Notes", onValueChange: next => { values.push(next); root.render(render(next)); } });
      await act(() => root.render(render("before")));
      update = async value => { await act(() => root.render(render(value))); };
      cleanup = async () => { await act(() => root.unmount()); };
    }
    const textarea = target.querySelector("textarea")!;
    await act(() => {
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(textarea, "first\nsecond");
      textarea.dispatchEvent(new Event("input", { bubbles: true })); flushSync();
    });
    expect(values).toEqual(["first\nsecond"]);
    expect(new FormData(target).get("notes")).toBe("first\nsecond");
    await update("");
    expect(textarea.value).toBe("");
    expect(textarea.getAttribute("aria-label")).toBe("Notes");
  });

  it("Input clears a controlled number when its model becomes null", async () => {
    const { target, update } = await renderInput(framework, "number", 7, () => {});
    expect(target.querySelector("input")!.value).toBe("7");
    await update(null);
    expect(target.querySelector("input")!.value).toBe("");
  });
});
