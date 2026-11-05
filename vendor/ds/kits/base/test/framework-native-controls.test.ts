import { mount, unmount, flushSync } from "svelte";
import { act, createElement as h, createRef, useState, type ComponentType } from "react";
import { createRoot } from "react-dom/client";
import { renderToString } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import * as react from "../react/src";
import Fixture from "./fixtures/FrameworkNativeControls.svelte";
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
type Name = "ToggleButton" | "Rating" | "FileInput";
const base = { ToggleButton: { label: "Pin review" }, Rating: { legend: "Release confidence", name: "confidence" }, FileInput: { label: "Evidence", name: "evidence" } };
function svelteProps(options: Record<string, unknown>) {
  return Object.fromEntries(Object.entries(options).map(([key, value]) => [key === "className" ? "class" : key.startsWith("on") ? key.toLowerCase() : key, value]));
}
async function render(name: Name, options: Record<string, unknown> = {}) {
  const a = document.createElement("div"), b = document.createElement("div"); document.body.append(a, b);
  const props = (next: Record<string, unknown>) => ({ ...base[name], className: "consumer-extra", ...next });
  const component = mount(Fixture, { target: a, props: { name, options: svelteProps(props(options)) } }); flushSync();
  const root = createRoot(b);
  await act(() => root.render(h("form", null, h(react[name] as ComponentType<any>, props(options)))));
  return { a, b, update: async (next: Record<string, unknown>) => {
    flushSync(() => component.update(svelteProps(props(next))));
    await act(() => root.render(h("form", null, h(react[name] as ComponentType<any>, props(next)))));
  }, cleanup: async () => { await act(() => root.unmount()); await unmount(component); a.remove(); b.remove(); } };
}
function snapshot(target: HTMLElement) {
  const ids = [...target.querySelectorAll("[id]")].map(el => el.id);
  const normalize = (v: string) => v.split(" ").map(token => ids.includes(token) ? `id-${ids.indexOf(token)}` : token).join(" ");
  return [...target.querySelectorAll("*")].map(el => ({ tag: el.tagName, classes: [...el.classList].sort(),
    attrs: Object.fromEntries([...el.attributes].filter(a => !["class", "checked"].includes(a.name) && !a.name.startsWith("data-svelte"))
      .map(a => [a.name, ["id", "for", "aria-describedby", "aria-errormessage"].includes(a.name) ? normalize(a.value) : a.name.startsWith("data-") && a.value === "true" ? "" : a.value]).sort()),
    checked: el instanceof HTMLInputElement && el.type === "radio" ? el.checked : undefined,
    text: el.children.length ? undefined : el.textContent?.trim() }));
}
const scenarios: Array<[Name, Record<string, unknown>]> = [
  ...[false, true].flatMap(defaultPressed => [false, true].map(allowUnpress => ["ToggleButton", { defaultPressed, allowUnpress }] as [Name, Record<string, unknown>])),
  ["ToggleButton", { pressed: true, disabled: true }],
  ...[undefined, 0, 3, 5].map(defaultValue => ["Rating", { defaultValue }] as [Name, Record<string, unknown>]),
  ...[0, -3, 2.9, NaN, Infinity].map(max => ["Rating", { max }] as [Name, Record<string, unknown>]),
  ["Rating", { value: 2, help: "Shared with the team", error: "Choose a rating", required: true, disabled: true, id: "rating", optionClass: "option-extra", controlClass: "control-extra", markClass: "mark-extra", outputClass: "output-extra" }],
  ["FileInput", {}], ["FileInput", { multiple: true, required: true, accept: ".txt", help: "Plain text", error: "Select evidence", fieldClass: "field-extra", disabled: true }],
];
for (const [name, options] of scenarios) it(`matches ${name} content, native semantics and shared appearance ${JSON.stringify(options)}`, async () => {
  const r = await render(name, options);
  try { expect(snapshot(r.b)).toEqual(snapshot(r.a)); } finally { await r.cleanup(); }
});
for (const framework of ["svelte", "react"] as const) describe(`${framework} native control scenarios`, () => {
  it("retains uncontrolled toggle state over unrelated updates and honors locked/disabled selection", async () => {
    const change = vi.fn(), click = vi.fn(); const r = await render("ToggleButton", { onPressedChange: change, onClick: click });
    const target = framework === "react" ? r.b : r.a;
    try {
      const button = target.querySelector("button")!;
      await act(async () => { button.click(); flushSync(); });
      expect(button.getAttribute("aria-pressed")).toBe("true"); expect(change).toHaveBeenLastCalledWith(true);
      await r.update({ defaultPressed: false, allowUnpress: false, onPressedChange: change, onClick: click });
      await act(async () => { button.click(); flushSync(); });
      expect(button.getAttribute("aria-pressed")).toBe("true"); expect(change).toHaveBeenCalledTimes(1); expect(click).toHaveBeenCalledTimes(2);
      await r.update({ pressed: true, disabled: true, onPressedChange: change, onClick: click });
      await act(async () => { button.click(); flushSync(); });
      expect(click).toHaveBeenCalledTimes(2); expect(button.type).toBe("button");
    } finally { await r.cleanup(); }
  });
  it("serializes native rating values, keeps associations stable and resets the readout", async () => {
    const change = vi.fn(); const r = await render("Rating", { defaultValue: 2, help: "Visible help", onValueChange: change });
    const target = framework === "react" ? r.b : r.a;
    try {
      const form = target.querySelector("form")!; const controls = target.querySelectorAll<HTMLInputElement>('input[type="radio"]'); const id = controls[0]!.id;
      await act(async () => { controls[4]!.click(); flushSync(); });
      expect(new FormData(form).get("confidence")).toBe("5"); expect(target.querySelector("output")?.textContent).toBe("5 of 5");
      expect(change).toHaveBeenCalledWith(5);
      await r.update({ defaultValue: 4, error: "Please review", onValueChange: change });
      expect(controls[0]!.id).toBe(id); expect(controls[0]!.getAttribute("aria-errormessage")).toBe(target.querySelector('[role="alert"]')!.id);
      await act(async () => { form.reset(); await new Promise(resolve => setTimeout(resolve, 0)); flushSync(); });
      expect(new FormData(form).get("confidence")).toBe("2"); expect(target.querySelector("output")?.textContent).toBe("2 of 5");
      expect(change).toHaveBeenCalledTimes(1);
      await act(async () => { form.reset(); await new Promise(resolve => setTimeout(resolve, 0)); flushSync(); });
      expect(new FormData(form).get("confidence")).toBe("2");
    } finally { await r.cleanup(); }
  });
  it("respects canceled rating resets and native disabled-fieldset submission", async () => {
    const r = await render("Rating", { defaultValue: 2 }); const target = framework === "react" ? r.b : r.a;
    try {
      const form = target.querySelector("form")!;
      await act(async () => { target.querySelectorAll<HTMLInputElement>("input")[4]!.click(); flushSync(); });
      form.addEventListener("reset", event => event.preventDefault(), { once: true });
      await act(async () => { form.reset(); await new Promise(resolve => setTimeout(resolve, 0)); flushSync(); });
      expect(new FormData(form).get("confidence")).toBe("5");
      await r.update({ disabled: true, defaultValue: 2 });
      expect(new FormData(form).has("confidence")).toBe(false);
    } finally { await r.cleanup(); }
  });
  it("reports selected filenames and the native change event", async () => {
    const change = vi.fn(), filesChange = vi.fn(); const r = await render("FileInput", { multiple: true, onChange: change, onFilesChange: filesChange });
    const target = framework === "react" ? r.b : r.a;
    try {
      const input = target.querySelector<HTMLInputElement>("input")!;
      const files = Object.assign([new File(["trace"], "trace.txt"), new File(["report"], "report.txt")], { item: (i: number) => files[i] ?? null });
      Object.defineProperty(input, "files", { configurable: true, writable: true, value: files });
      await act(async () => { input.dispatchEvent(new Event("change", { bubbles: true })); flushSync(); });
      expect(target.querySelector("[data-file-name]")?.textContent?.trim()).toBe("trace.txt, report.txt");
      expect(change).toHaveBeenCalledTimes(1); expect(filesChange).toHaveBeenCalledWith(files);
      input.focus(); expect(document.activeElement).toBe(input);
    } finally { await r.cleanup(); }
  });
});
it("delivers the pressed change before the native click callback", async () => {
  const order: string[] = [];
  const r = await render("ToggleButton", { onPressedChange: () => order.push("change"), onClick: () => order.push("click") });
  try {
    const button = r.b.querySelector("button")!;
    expect(button.textContent).toBe("Pin review");
    await act(() => button.click()); expect(order).toEqual(["change", "click"]);
  } finally { await r.cleanup(); }
});
it("keeps controlled React toggle and rating values caller-owned until updated", async () => {
  for (const name of ["ToggleButton", "Rating"] as const) {
    const change = vi.fn(); const r = await render(name, name === "ToggleButton" ? { pressed: false, onPressedChange: change } : { value: 2, onValueChange: change });
    try {
      await act(() => (name === "ToggleButton" ? r.b.querySelector("button")! : r.b.querySelectorAll<HTMLInputElement>("input")[4]!).click());
      expect(change).toHaveBeenCalledWith(name === "ToggleButton" ? true : 5);
      if (name === "ToggleButton") expect(r.b.querySelector("button")!.getAttribute("aria-pressed")).toBe("false");
      else expect(new FormData(r.b.querySelector("form")!).get("confidence")).toBe("2");
      await r.update(name === "ToggleButton" ? { pressed: true } : { value: 4 });
      if (name === "ToggleButton") expect(r.b.querySelector("button")!.getAttribute("aria-pressed")).toBe("true");
      else {
        await act(async () => { r.b.querySelector("form")!.reset(); await new Promise(resolve => setTimeout(resolve, 0)); });
        expect(new FormData(r.b.querySelector("form")!).get("confidence")).toBe("4");
      }
    } finally { await r.cleanup(); }
  }
});
it("forwards actual React refs and renders useful native SSR without controlled-value warnings", async () => {
  const error = vi.spyOn(console, "error");
  try {
    for (const name of ["ToggleButton", "Rating", "FileInput"] as const) {
      const ref = createRef<HTMLElement>(); const r = await render(name, { ref });
      try { expect(ref.current?.tagName).toBe(name === "ToggleButton" ? "BUTTON" : name === "Rating" ? "FIELDSET" : "INPUT"); } finally { await r.cleanup(); }
      const html = renderToString(h(react[name] as ComponentType<any>, base[name]));
      expect(html).toContain(name === "ToggleButton" ? 'aria-pressed="false"' : name === "Rating" ? "0 of 5" : 'type="file"');
    }
    expect(error).not.toHaveBeenCalled();
  } finally { error.mockRestore(); }
});

it("honors a controlled React rating updated by the form reset handler", async () => {
  function Consumer() {
    const [value, setValue] = useState(2);
    return h("form", { onReset: () => setValue(4) }, h(react.Rating, { legend: "Confidence", name: "confidence", value, onValueChange: setValue }));
  }
  const target = document.createElement("div"); document.body.append(target); const root = createRoot(target);
  try {
    await act(() => root.render(h(Consumer)));
    await act(async () => { target.querySelector("form")!.reset(); await new Promise(resolve => setTimeout(resolve, 0)); });
    expect(new FormData(target.querySelector("form")!).get("confidence")).toBe("4");
    expect(target.querySelector("output")!.textContent).toBe("4 of 5");
  } finally { await act(() => root.unmount()); target.remove(); }
});
