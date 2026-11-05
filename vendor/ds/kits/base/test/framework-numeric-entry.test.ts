import { mount, unmount, flushSync, tick } from "svelte";
import { act, createElement as h, createRef, type ComponentType } from "react";
import { createRoot, hydrateRoot } from "react-dom/client";
import { renderToString } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import * as react from "../react/src";
import Fixture from "./fixtures/FrameworkNumericEntry.svelte";
import { rangeValue } from "../src/core/slider";
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
type Name = "NumberField" | "Slider" | "OneTimeCodeInput";
async function render(name: Name, options: Record<string, any> = {}) {
  const a = document.createElement("div"), b = document.createElement("div"); document.body.append(a, b);
  const changed = { svelte: vi.fn(), react: vi.fn() }, complete = { svelte: vi.fn(), react: vi.fn() };
  const base = { label: "Capacity", ...options };
  const svelte = mount(Fixture, { target: a, props: { name, options: base, changed: changed.svelte, complete: complete.svelte } }); flushSync();
  const root = createRoot(b);
  const element = (next: Record<string, any>) => {
    const { inputmode, class: className, readonly, ...native } = next;
    return h("form", { id: "numeric-form" }, h(react[name] as ComponentType<any>, { ...native, ...(inputmode ? { inputMode: inputmode } : {}), className, readOnly: readonly, onValueChange: changed.react, ...(name === "OneTimeCodeInput" ? { onComplete: complete.react } : {}) }));
  };
  await act(() => root.render(element(base)));
  return { a, b, changed, complete, update: async (next: Record<string, any>) => { const props = { label: "Capacity", ...next }; flushSync(() => svelte.update(props)); await act(() => root.render(element(props))); }, cleanup: async () => { await act(() => root.unmount()); await unmount(svelte); a.remove(); b.remove(); } };
}
async function interact(fn: () => void) { await act(async () => { fn(); flushSync(); await tick(); }); }
async function input(element: HTMLInputElement, value: string) {
  await interact(() => { Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(element, value); element.dispatchEvent(new Event("input", { bubbles: true })); });
}
function snapshot(target: HTMLElement) {
  const ids = [...target.querySelectorAll("[id]")].map(element => element.id);
  const normalize = (value: string) => value.split(" ").map(token => ids.includes(token) ? `id-${ids.indexOf(token)}` : token).join(" ");
  return [...target.querySelectorAll("*")].map(element => ({ tag: element.tagName, classes: [...element.classList].sort(),
    attributes: Object.fromEntries([...element.attributes].filter(a => !["class", "style", "value", "defaultvalue"].includes(a.name) && !a.name.startsWith("data-svelte"))
      .map(a => [a.name, ["id", "for", "aria-labelledby", "aria-describedby", "aria-errormessage"].includes(a.name) ? normalize(a.value) : a.name.startsWith("data-") && a.value === "true" ? "" : a.value]).sort()),
    value: element instanceof HTMLInputElement ? element.value : undefined,
    text: element.children.length ? undefined : element.textContent?.trim() }));
}
const scenarios: [Name, Record<string, unknown>][] = [
  ["NumberField", {}], ["NumberField", { defaultValue: 0.3, min: 0, max: 0.5, step: 0.1, help: "Sampling fraction" }],
  ["NumberField", { defaultValue: 20, min: 1, max: 12, error: "Out of range", required: true }],
  ["NumberField", { value: null, controlSize: "sm", decrementLabel: "Less capacity", incrementLabel: "More capacity" }],
  ["NumberField", { defaultValue: 3, disabled: true }],
  ["Slider", {}], ["Slider", { min: 10, max: 90, step: 10, defaultValue: 40, formatValue: (v: number) => `${v}%`, help: "Deployment capacity" }],
  ["Slider", { min: 10, max: 90, defaultValue: -10 }], ["Slider", { defaultValue: 130, error: "Review", required: true }],
  ["Slider", { min: 0, max: 1, step: 0.1, defaultValue: 0.26 }], ["Slider", { min: 0, max: 1, step: "any", defaultValue: 0.26 }],
  ["OneTimeCodeInput", {}], ["OneTimeCodeInput", { name: "code", defaultValue: "012345", required: true, help: "Six digits" }],
  ["OneTimeCodeInput", { length: 4, value: "AB12", inputmode: "text", pattern: "[A-Z0-9]", error: "Check code" }],
  ["OneTimeCodeInput", { length: 4, defaultValue: "12345", disabled: true, segmentClass: "consumer-segment" }],
];
for (const [name, options] of scenarios) it(`matches ${name} semantics, appearance and native initial values ${JSON.stringify(options)}`, async () => {
  const r = await render(name, options); try { expect(snapshot(r.b)).toEqual(snapshot(r.a)); } finally { await r.cleanup(); }
});
for (const framework of ["svelte", "react"] as const) describe(`${framework} native numeric and code behavior`, () => {
  it("steps decimals once, retains the initial native step base and serializes invalid edits", async () => {
    const r = await render("NumberField", { name: "units", defaultValue: 0.3, step: 0.2 });
    try {
      const target = framework === "react" ? r.b : r.a, field = target.querySelector("input")!;
      await interact(() => target.querySelectorAll("button")[1]!.click());
      expect(field.value).toBe("0.5"); expect(r.changed[framework]).toHaveBeenCalledExactlyOnceWith(0.5);
      await input(field, "0.4"); expect(field.validity.stepMismatch).toBe(true);
      expect(new FormData(target.querySelector("form")!).get("units")).toBe("0.4");
      field.blur(); expect(field.defaultValue).toBe("0.3");
      await input(field, ""); expect(r.changed[framework]).toHaveBeenLastCalledWith(null);
      await r.update({ name: "units", defaultValue: 0.3, step: 0.2, min: 0, max: 0.5 });
      await input(field, "0.5"); expect(target.querySelectorAll("button")[1]!.disabled).toBe(true);
    } finally { await r.cleanup(); }
  });
  for (const name of ["NumberField", "Slider", "OneTimeCodeInput"] as const) it(`${name}: follows repeated and canceled native resets, then accepts another change`, async () => {
    const defaults = name === "OneTimeCodeInput" ? { defaultValue: "12", length: 4 } : { defaultValue: 3 };
    const r = await render(name, { ...defaults, name: "entry" });
    try {
      const target = framework === "react" ? r.b : r.a, field = target.querySelector<HTMLInputElement>("input")!, form = target.querySelector("form")!;
      await act(async () => { form.reset(); await new Promise(resolve => setTimeout(resolve, 5)); flushSync(); await tick(); });
      expect(field.value).toBe(name === "OneTimeCodeInput" ? "1" : "3");
      if (name === "OneTimeCodeInput") expect([...target.querySelectorAll<HTMLInputElement>("[data-otp-segment]")].map(input => input.value).join("")).toBe("12");
      for (let repeat = 0; repeat < 2; repeat++) {
        await input(field, "7");
        const beforeReset = r.changed[framework].mock.calls.length;
        await act(async () => { form.reset(); await new Promise(resolve => setTimeout(resolve, 5)); flushSync(); await tick(); });
        expect(field.value).toBe(name === "OneTimeCodeInput" ? "1" : "3");
        expect(r.changed[framework].mock.calls.length).toBe(beforeReset + (name === "NumberField" ? 1 : 0));
        expect(new FormData(form).get("entry")).toBe(name === "OneTimeCodeInput" ? "12" : "3");
        await input(field, "8"); expect(field.value).toBe("8");
      }
      form.addEventListener("reset", event => event.preventDefault(), { once: true });
      await act(async () => { form.reset(); await new Promise(resolve => setTimeout(resolve, 5)); flushSync(); });
      expect(field.value).toBe("8");
    } finally { await r.cleanup(); }
  });
  it("distributes code paste, keeps empty positions, navigates, and preserves native pattern validity", async () => {
    const r = await render("OneTimeCodeInput", { name: "code", length: 4, required: true });
    try {
      const target = framework === "react" ? r.b : r.a, fields = [...target.querySelectorAll<HTMLInputElement>("[data-otp-segment]")];
      await input(fields[2]!, "7"); expect(fields.map(f => f.value)).toEqual(["", "", "7", ""]);
      expect(document.activeElement).toBe(fields[3]);
      await interact(() => { const event = new Event("paste", { bubbles: true, cancelable: true }); Object.defineProperty(event, "clipboardData", { value: { getData: () => "0 1-2B" } }); fields[0]!.dispatchEvent(event); });
      expect(fields.map(f => f.value).join("")).toBe("012B"); expect(new FormData(target.querySelector("form")!).get("code")).toBe("012B");
      expect(r.complete[framework]).toHaveBeenLastCalledWith("012B"); expect(fields[3]!.validity.patternMismatch).toBe(true);
      await interact(() => fields[3]!.dispatchEvent(new KeyboardEvent("keydown", { key: "Home", bubbles: true, cancelable: true }))); expect(document.activeElement).toBe(fields[0]);
      await input(fields[0]!, "#"); expect(fields[0]!.value).toBe(""); expect(fields[0]!.validity.valueMissing).toBe(true);
    } finally { await r.cleanup(); }
  });
});
it("keeps caller-owned React values until the caller accepts or replaces them", async () => {
  for (const name of ["NumberField", "Slider", "OneTimeCodeInput"] as const) {
    const r = await render(name, { value: name === "OneTimeCodeInput" ? "12" : 3, length: 4 });
    try {
      const field = r.b.querySelector<HTMLInputElement>("input")!; await input(field, "8");
      expect(field.value).toBe(name === "OneTimeCodeInput" ? "1" : "3");
      await r.update({ value: name === "OneTimeCodeInput" ? "89" : 9, length: 4 }); expect(field.value).toBe(name === "OneTimeCodeInput" ? "8" : "9");
    } finally { await r.cleanup(); }
  }
});
for (const [name, options] of [scenarios[0]!, ...scenarios.filter(([name]) => name === "OneTimeCodeInput" || name === "Slider")]) it(`hydrates useful ${name} SSR without replacing native fields`, async () => {
  const target = document.createElement("div"); document.body.append(target);
  const ref = createRef<any>(), component = h(react[name] as ComponentType<any>, { label: "Capacity", ...options, ref });
  target.innerHTML = renderToString(component); const original = target.querySelector("input"), errors: unknown[] = [];
  let root: ReturnType<typeof hydrateRoot>;
  await act(() => { root = hydrateRoot(target, component, { onRecoverableError: error => errors.push(error) }); });
  try { expect(target.querySelector("input")).toBe(original); expect(ref.current).not.toBeNull(); expect(errors).toEqual([]); }
  finally { await act(() => root!.unmount()); target.remove(); }
});
it("sanitizes range readouts before SSR, including nonaligned maxima and decimal ties", () => {
  for (const [value, min, max, step, expected] of [[0,10,90,1,10],[26,0,100,10,30],[25,0,100,10,30],[99,0,95,10,90],[0.26,0,1,0.1,0.3],[0,10,0,1,10]] as const) {
    expect(rangeValue(value,min,max,step)).toBe(expected);
  }
});

it("refuses invalid native number constraints and OTP lengths during SSR", () => {
  for (const props of [{min:Infinity},{max:NaN},{min:5,max:2},{step:0},{step:-1}]) expect(() => renderToString(h(react.NumberField,{label:"Capacity",...props}))).toThrow("NumberField:");
  for (const length of [0,-1,2.5,Infinity,NaN]) expect(() => renderToString(h(react.OneTimeCodeInput,{label:"Code",length}))).toThrow("OneTimeCodeInput:");
});
for (const framework of ["svelte", "react"] as const) for (const name of ["NumberField", "Slider", "OneTimeCodeInput"] as const) it(`${framework} ${name}: follows reassociated native form ownership on reset`, async () => {
  const first = document.createElement("form"), second = document.createElement("form"); first.id = `first-${framework}-${name}`; second.id = `second-${framework}-${name}`; document.body.append(first, second);
  const props = { name: "entry", defaultValue: name === "OneTimeCodeInput" ? "12" : 3, length: 4, form: first.id };
  const r = await render(name, props);
  try {
    const target = framework === "react" ? r.b : r.a, field = target.querySelector<HTMLInputElement>("input")!;
    await r.update({ ...props, form: second.id }); await input(field, "7");
    await act(async () => { first.reset(); await new Promise(resolve => setTimeout(resolve, 5)); flushSync(); }); expect(field.value).toBe("7");
    await act(async () => { second.reset(); await new Promise(resolve => setTimeout(resolve, 5)); flushSync(); });
    expect(field.value).toBe(name === "OneTimeCodeInput" ? "1" : "3");
    const initial = name === "OneTimeCodeInput" ? "12" : "3";
    expect(new FormData(second).getAll("entry")).toEqual([initial, initial]);
    if (name === "OneTimeCodeInput") expect([...target.querySelectorAll<HTMLInputElement>("[data-otp-segment]")].map(input => input.value).join("")).toBe("12");
  } finally { await r.cleanup(); first.remove(); second.remove(); }
});
