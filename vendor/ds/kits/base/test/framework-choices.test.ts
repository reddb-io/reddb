import { mount, unmount, flushSync } from "svelte";
import { act, createElement as h, createRef, useState, type ComponentType } from "react";
import { createRoot } from "react-dom/client";
import { renderToString } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import * as react from "../react/src";
import Fixture from "./fixtures/FrameworkChoices.svelte";
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
type Name = "Checkbox" | "Switch" | "RadioGroup" | "Select";
const options = [{ value: "sa", label: "South America", description: "Primary region" }, { value: "us", label: "North America", description: "Read replica" }, { value: "disabled", label: "Unavailable", description: "No capacity", disabled: true }];
const base = { Checkbox: { label: "Share diagnostics", name: "diagnostics" }, Switch: { label: "Email digest", name: "digest" }, RadioGroup: { legend: "Region", name: "region", options }, Select: { "aria-label": "Region", name: "region", options } };
function native(options: Record<string, unknown>) { return Object.fromEntries(Object.entries(options).map(([key, value]) => [key === "className" ? "class" : key.startsWith("on") ? key.toLowerCase() : key, value])); }
function props(name: Name, options: Record<string, unknown>, rich: boolean) {
  const shared = { ...base[name], className: "consumer-extra", ...options };
  if (name === "RadioGroup" && rich) return { ...shared, optionMedia: (option: react.RadioGroupOption) => h("span", { "aria-hidden": true }, option.value) };
  if (name === "Select" && rich) return { ...shared, children: h("optgroup", { label: "More regions" }, h("option", { value: "eu" }, "Europe"), h("option", { value: "ap", disabled: true }, "Asia Pacific")) };
  return shared;
}
async function render(name: Name, supplied: Record<string, unknown> = {}, rich = false) {
  const a = document.createElement("div"), b = document.createElement("div"); document.body.append(a, b);
  const component = mount(Fixture, { target: a, props: { name, options: native({ ...base[name], className: "consumer-extra", ...supplied }), rich } }); flushSync();
  const root = createRoot(b);
  await act(() => root.render(h("form", null, h(react[name] as ComponentType<any>, props(name, supplied, rich)))));
  return { a, b, update: async (options: Record<string, unknown>) => {
    flushSync(() => component.update(native({ ...base[name], className: "consumer-extra", ...options })));
    await act(() => root.render(h("form", null, h(react[name] as ComponentType<any>, props(name, options, rich)))));
  }, cleanup: async () => { await act(() => root.unmount()); await unmount(component); a.remove(); b.remove(); } };
}
function snapshot(target: HTMLElement) {
  const ids = [...target.querySelectorAll("[id]")].map(el => el.id);
  const normalize = (v: string) => v.split(" ").map(token => ids.includes(token) ? `id-${ids.indexOf(token)}` : token).join(" ");
  return [...target.querySelectorAll("*")].map(el => ({ tag: el.tagName, classes: [...el.classList].sort(),
    attrs: Object.fromEntries([...el.attributes].filter(a => !["class", "checked", "selected"].includes(a.name) && !a.name.startsWith("data-svelte"))
      .map(a => [a.name, ["id", "for", "aria-labelledby", "aria-describedby", "aria-errormessage"].includes(a.name) ? normalize(a.value) : a.name.startsWith("data-") && a.value === "true" ? "" : a.value]).sort()),
    checked: el instanceof HTMLInputElement ? el.checked : undefined,
    selected: el instanceof HTMLOptionElement ? el.selected : undefined,
    text: el.children.length ? undefined : el.textContent?.trim() }));
}
const cases: Array<[Name, Record<string, unknown>?, boolean?]> = [
  ...(["Checkbox", "Switch"] as const).flatMap(name => [undefined, false, true].flatMap(checked => [false, true].map(disabled => [name, { checked, disabled, required: true, help: "Shared help", error: "Review this choice", value: "enabled", fieldClass: "field-extra" }] as [Name, Record<string, unknown>]))),
  ["Checkbox", { defaultChecked: true }], ["Switch", { defaultChecked: true }],
  ["RadioGroup"], ["RadioGroup", { defaultValue: "us" }], ["RadioGroup", { value: "sa", disabled: true, required: true, help: "Shared help", error: "Choose a region", optionClass: "option-extra", controlClass: "control-extra" }],
  ["RadioGroup", { defaultValue: "us", variant: "cards" }, true], ["RadioGroup", { options: [] }],
  ["Select", { value: "missing" }],
  ["Select"], ["Select", { defaultValue: "us" }], ["Select", { value: "us", controlSize: "sm", disabled: true, required: true }],
  ["Select", { multiple: true, defaultValue: ["sa", "eu"], size: 4 }, true], ["Select", { defaultValue: "eu", controlSize: "lg" }, true],
];
for (const [name, options, rich] of cases) it(`matches ${name} shared appearance, native selection and associations ${JSON.stringify(options ?? {})}`, async () => {
  const r = await render(name, options, rich);
  try { expect(snapshot(r.b)).toEqual(snapshot(r.a)); } finally { await r.cleanup(); }
});
async function reset(form: HTMLFormElement) { await act(async () => { form.reset(); await new Promise(resolve => setTimeout(resolve, 0)); flushSync(); }); }
for (const framework of ["svelte", "react"] as const) describe(`${framework} native choice contracts`, () => {
  for (const name of ["Checkbox", "Switch"] as const) it(`${name} retains native state, callback ordering, required/disabled and reset`, async () => {
    const order: string[] = []; const state = vi.fn();
    const supplied = { defaultChecked: false, required: true, value: "enabled", onChange: () => order.push("native"), onCheckedChange: (next: boolean) => { order.push("checked"); state(next); } };
    const r = await render(name, supplied); const target = framework === "react" ? r.b : r.a;
    try {
      const input = target.querySelector<HTMLInputElement>("input")!, form = target.querySelector("form")!;
      expect(input.checkValidity()).toBe(false);
      await act(async () => { input.click(); flushSync(); });
      expect(input.checked).toBe(true); expect(state).toHaveBeenCalledWith(true); expect(order).toEqual(["native", "checked"]);
      expect(new FormData(form).get(input.name)).toBe("enabled");
      await r.update({ ...supplied, help: "Updated help" }); expect(input.checked).toBe(true);
      await reset(form); expect(input.checked).toBe(false); expect(state).toHaveBeenCalledTimes(1);
      await act(async () => { input.click(); flushSync(); });
      form.addEventListener("reset", event => event.preventDefault(), { once: true }); await reset(form); expect(input.checked).toBe(true);
      await r.update({ ...supplied, disabled: true }); expect(new FormData(form).has(input.name)).toBe(false);
      await act(async () => { input.click(); flushSync(); }); expect(state).toHaveBeenCalledTimes(2);
    } finally { await r.cleanup(); }
  });
  it("keeps RadioGroup's names exclusive, relationships stable and resets the default", async () => {
    const change = vi.fn(); const r = await render("RadioGroup", { defaultValue: "sa", help: "Group help", onValueChange: change }, true); const target = framework === "react" ? r.b : r.a;
    try {
      const form = target.querySelector("form")!, radios = target.querySelectorAll<HTMLInputElement>("input"), id = radios[0]!.id;
      await act(async () => { radios[1]!.click(); flushSync(); });
      expect(new FormData(form).get("region")).toBe("us"); expect(change).toHaveBeenCalledWith("us");
      await r.update({ defaultValue: "sa", error: "Review region", onValueChange: change });
      expect(radios[0]!.id).toBe(id); expect(radios[0]!.getAttribute("aria-errormessage")).toBe(target.querySelector('[role="alert"]')!.id);
      expect(radios[0]!.getAttribute("aria-labelledby")).toBe(`${id}-label`);
      await reset(form); expect(new FormData(form).get("region")).toBe("sa"); expect(change).toHaveBeenCalledTimes(1);
      await reset(form); expect(new FormData(form).get("region")).toBe("sa");
      await r.update({ disabled: true, defaultValue: "sa" }); expect(new FormData(form).has("region")).toBe(false);
    } finally { await r.cleanup(); }
  });
  it("submits all multiple selections while keeping the callback's canonical first-value dialect", async () => {
    const change = vi.fn(), nativeChange = vi.fn(); const r = await render("Select", { multiple: true, defaultValue: ["sa", "eu"], onChange: nativeChange, onValueChange: change }, true); const target = framework === "react" ? r.b : r.a;
    try {
      const form = target.querySelector("form")!, select = target.querySelector("select")!;
      expect(new FormData(form).getAll("region")).toEqual(["sa", "eu"]);
      await act(async () => { for (const option of select.options) option.selected = option.value === "us" || option.value === "eu"; select.dispatchEvent(new Event("change", { bubbles: true })); flushSync(); });
      expect(change).toHaveBeenCalledWith("us"); expect(nativeChange).toHaveBeenCalledTimes(1); expect(new FormData(form).getAll("region")).toEqual(["us", "eu"]);
      await r.update({ multiple: true, defaultValue: ["sa", "eu"], help: undefined }); expect(new FormData(form).getAll("region")).toEqual(["us", "eu"]);
      await reset(form); expect(new FormData(form).getAll("region")).toEqual(["sa", "eu"]); expect(change).toHaveBeenCalledTimes(1);
    } finally { await r.cleanup(); }
  });
});
it("keeps an explicit removed Select value empty in both native adapters", async () => {
  const r = await render("Select", { value: "us", required: true });
  try {
    await r.update({ value: "us", options: options.slice(0, 1), required: true });
    for (const target of [r.a, r.b]) {
      const select = target.querySelector("select")!;
      expect(select.selectedIndex).toBe(-1); expect(select.checkValidity()).toBe(false);
      expect(new FormData(target.querySelector("form")!).has("region")).toBe(false);
    }
  } finally { await r.cleanup(); }
});
it("keeps controlled React choices caller-owned until props update, including native reset", async () => {
  for (const name of ["Checkbox", "Switch", "RadioGroup", "Select"] as const) {
    const change = vi.fn(); const binary = name === "Checkbox" || name === "Switch";
    const r = await render(name, binary ? { checked: false, onCheckedChange: change } : { value: "sa", onValueChange: change });
    try {
      await act(() => {
        if (binary) r.b.querySelector<HTMLInputElement>("input")!.click();
        else if (name === "RadioGroup") r.b.querySelectorAll<HTMLInputElement>("input")[1]!.click();
        else { const select = r.b.querySelector("select")!; select.value = "us"; select.dispatchEvent(new Event("change", { bubbles: true })); }
      });
      expect(change).toHaveBeenCalledWith(binary ? true : "us");
      expect(new FormData(r.b.querySelector("form")!).get(binary ? name === "Checkbox" ? "diagnostics" : "digest" : "region")).toBe(binary ? null : "sa");
      await r.update(binary ? { checked: true } : { value: "us" }); await reset(r.b.querySelector("form")!);
      expect(new FormData(r.b.querySelector("form")!).get(binary ? name === "Checkbox" ? "diagnostics" : "digest" : "region")).toBe(binary ? "on" : "us");
    } finally { await r.cleanup(); }
  }
});
it("forwards native React refs and renders selected options and labels during SSR", async () => {
  const error = vi.spyOn(console, "error");
  try {
    for (const name of ["Checkbox", "Switch", "RadioGroup", "Select"] as const) {
      const ref = createRef<HTMLElement>(); const r = await render(name, { ref });
      try { expect(ref.current?.tagName).toBe(name === "RadioGroup" ? "FIELDSET" : name === "Select" ? "SELECT" : "INPUT"); } finally { await r.cleanup(); }
      const html = renderToString(h(react[name] as ComponentType<any>, { ...base[name], ...(name === "RadioGroup" || name === "Select" ? { defaultValue: "us" } : { defaultChecked: true }) }));
      expect(html).toContain(name === "RadioGroup" || name === "Select" ? "us" : 'checked=""');
    }
    expect(error).not.toHaveBeenCalled();
  } finally { error.mockRestore(); }
});
