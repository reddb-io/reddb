import { mount, unmount, flushSync } from "svelte";
import { act, createElement as h, createRef, type ComponentType } from "react";
import { createRoot, hydrateRoot } from "react-dom/client";
import { renderToString } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";
import * as react from "../react/src";
import Fixture from "./fixtures/FrameworkIdentityControls.svelte";
import { appearanceStorageKey } from "../src/appearance-switch.behavior";
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
type Name = "Avatar" | "ToggleGroup" | "AppearanceSwitch";
const options = [{ value: "all", label: "All" }, { value: "blocked", label: "Unavailable", disabled: true }, { value: "assigned", label: "Assigned" }, { value: "unread", label: "Unread" }];
const appearanceOptions = [{ value: "application", label: "Application" }, { value: "blocked", label: "Unavailable", disabled: true }, { value: "marketing", label: "Marketing" }];
const base = { Avatar: { name: "Ada Lovelace", fallback: "AL" }, ToggleGroup: { legend: "Conversations", name: "view", options }, AppearanceSwitch: { label: "Theme", axis: "theme", name: "appearance", options: appearanceOptions } };
const cleanups: (() => Promise<void>)[] = [];
afterEach(async () => { while (cleanups.length) await cleanups.pop()!(); });
function storage(initial: Record<string, string> = {}) {
  const values = new Map(Object.entries(initial));
  return { getItem: vi.fn((key: string) => values.get(key) ?? null), setItem: vi.fn((key: string, value: string) => { values.set(key, value); }) };
}
async function interact(fn: () => void) { await act(async () => { fn(); flushSync(); }); }
async function reset(form: HTMLFormElement) { await act(async () => { form.reset(); await new Promise(resolve => setTimeout(resolve, 0)); flushSync(); }); }
async function render(name: Name, supplied: Record<string, any> = {}) {
  const a = document.createElement("div"), b = document.createElement("div"); document.body.append(a, b);
  const appearance = name === "AppearanceSwitch" ? { root: document.createElement("div"), storage: storage() } : {};
  const props = (next: Record<string, any>) => ({ ...base[name], ...appearance, ...next });
  const native = (next: Record<string, any>) => Object.fromEntries(Object.entries(props(next)).map(([key, value]) => [key === "className" ? "class" : key.startsWith("on") ? key.toLowerCase() : key, value]));
  const component = mount(Fixture, { target: a, props: { name, options: native(supplied) } }); flushSync();
  const root = createRoot(b);
  const element = (next: Record<string, any>) => h("form", null, h(react[name] as ComponentType<any>, props(next)));
  await act(() => root.render(element(supplied)));
  cleanups.push(async () => { await act(() => root.unmount()); await unmount(component); a.remove(); b.remove(); });
  return { a, b, appearance, update: async (next: Record<string, any>) => { flushSync(() => component.update(native(next))); await act(() => root.render(element(next))); } };
}
function snapshot(target: HTMLElement) {
  const ids = [...target.querySelectorAll("[id]")].map(el => el.id);
  const normalize = (v: string) => v.split(" ").map(token => ids.includes(token) ? `id-${ids.indexOf(token)}` : token).join(" ");
  return [...target.querySelectorAll("*")].map(el => ({ tag: el.tagName, classes: [...el.classList].sort(),
    attrs: Object.fromEntries([...el.attributes].filter(a => !["class", "selected", "value"].includes(a.name) && !a.name.startsWith("data-svelte"))
      .map(a => [a.name, ["id", "for", "aria-labelledby", "aria-describedby", "aria-errormessage"].includes(a.name) ? normalize(a.value) : a.name.startsWith("data-") && a.value === "true" ? "" : a.value]).sort()),
    value: el instanceof HTMLInputElement || el instanceof HTMLSelectElement ? el.value : undefined,
    text: el.children.length ? undefined : el.textContent?.trim() }));
}
for (const [name, supplied] of [
  ...(["sm", "md", "lg"] as const).map(size => ["Avatar", { size, src: "/ada.png", className: "consumer-avatar", imageClass: "consumer-image", fallbackClass: "consumer-fallback" }]),
  ["Avatar", {}], ["ToggleGroup", {}], ["ToggleGroup", { defaultValue: "unread" }],
  ["ToggleGroup", { value: "assigned", disabled: true, help: "Scope of the list", error: "Review your scope", className: "consumer-group", listClass: "consumer-list", optionClass: "consumer-option" }],
  ["ToggleGroup", { options: [] }], ["ToggleGroup", { options: options.map(option => ({ ...option, disabled: true })) }],
  ["AppearanceSwitch", {}], ["AppearanceSwitch", { defaultValue: "marketing", disabled: true, className: "consumer-select", id: "consumer-appearance" }],
  ["AppearanceSwitch", { options: [] }], ["AppearanceSwitch", { options: appearanceOptions.map(option => ({ ...option, disabled: true })) }],
] as [Name, Record<string, any>][]) it(`matches ${name} semantics, slots and native defaults ${JSON.stringify(supplied)}`, async () => {
  const r = await render(name, supplied); expect(snapshot(r.b)).toEqual(snapshot(r.a));
});
for (const flavor of ["svelte", "react"] as const) describe(`${flavor} identity and settings`, () => {
  it("recovers a replaced image after failure, retaining the named text alternative", async () => {
    const r = await render("Avatar", { src: "/missing.png" }), target = flavor === "react" ? r.b : r.a;
    expect(target.querySelector("img")!.style.aspectRatio).toBe("1 / 1");
    await interact(() => target.querySelector("img")!.dispatchEvent(new Event("error")));
    expect(target.querySelector("img")).toBeNull(); expect(target.querySelector('[role="img"]')?.getAttribute("aria-label")).toBe("Ada Lovelace");
    expect(target.textContent?.trim()).toBe("AL"); await r.update({ src: "/replacement.png", name: "Grace Hopper", fallback: "GH" });
    expect(target.querySelector("img")?.getAttribute("src")).toBe("/replacement.png"); expect(target.textContent?.trim()).toBe("GH");
    await r.update({ fallback: "GH" }); expect(target.querySelector("img")).toBeNull();
  });
  it("uses roving selection, skips disabled choices, wraps, and follows RTL", async () => {
    const changed = vi.fn(), r = await render("ToggleGroup", { onValueChange: changed }), target = flavor === "react" ? r.b : r.a;
    const buttons = [...target.querySelectorAll("button")];
    async function key(index: number, key: string) { await interact(() => buttons[index]!.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }))); }
    await key(0, "ArrowRight"); expect(document.activeElement).toBe(buttons[2]);
    expect(buttons.map(button => button.getAttribute("aria-pressed"))).toEqual(["false", "false", "true", "false"]);
    await key(2, "End"); expect(document.activeElement).toBe(buttons[3]); await key(3, "ArrowDown"); expect(document.activeElement).toBe(buttons[0]);
    await key(0, "ArrowUp"); expect(document.activeElement).toBe(buttons[3]); await key(3, "Home"); expect(document.activeElement).toBe(buttons[0]);
    await r.update({ onValueChange: changed, dir: "rtl" });
    // jsdom does not inherit computed direction, so put the actual inherited browser result on the list.
    (target.querySelector('[role="group"]') as HTMLElement).style.direction = "rtl";
    await key(0, "ArrowLeft"); expect(document.activeElement).toBe(buttons[2]);
    const count = changed.mock.calls.length; await interact(() => buttons[2]!.click()); expect(changed).toHaveBeenCalledTimes(count);
    expect(buttons.filter(button => button.tabIndex === 0)).toEqual([buttons[2]]);
  });
  it("serializes the selected value and resets without a user callback, including canceled reset", async () => {
    const changed = vi.fn(), r = await render("ToggleGroup", { defaultValue: "assigned", onValueChange: changed }), target = flavor === "react" ? r.b : r.a;
    const form = target.querySelector("form")!, buttons = [...target.querySelectorAll("button")];
    await interact(() => buttons[3]!.click()); expect(new FormData(form).get("view")).toBe("unread");
    form.addEventListener("reset", event => event.preventDefault(), { once: true }); await reset(form); expect(new FormData(form).get("view")).toBe("unread");
    await reset(form); expect(new FormData(form).get("view")).toBe("assigned"); expect(changed).toHaveBeenCalledTimes(1);
    await interact(() => buttons[0]!.click()); expect(changed).toHaveBeenLastCalledWith("all");
    await r.update({ disabled: true }); expect(new FormData(form).has("view")).toBe(false);
    await interact(() => buttons[3]!.click()); expect(changed).toHaveBeenCalledTimes(2);
  });
  it("routes an external form and its reset to the same group", async () => {
    const external = document.createElement("form"); external.id = `external-${flavor}`; document.body.append(external);
    try {
      const r = await render("ToggleGroup", { form: external.id, defaultValue: "assigned" }), target = flavor === "react" ? r.b : r.a;
      (flavor === "react" ? r.a : r.b).querySelector<HTMLInputElement>('input[type="hidden"]')!.disabled = true;
      await interact(() => target.querySelectorAll("button")[3]!.click()); expect(new FormData(external).get("view")).toBe("unread");
      await reset(external); expect(new FormData(external).get("view")).toBe("assigned");
    } finally { external.remove(); }
  });
  it("restores enabled stored appearance, preserves other axes and handles blocked storage", async () => {
    const root = document.createElement("div"); root.dataset.colorScheme = "dark"; root.dataset.density = "compact";
    const persistence = storage({ [appearanceStorageKey("theme")]: "marketing" }), changed = vi.fn();
    const r = await render("AppearanceSwitch", { root, storage: persistence, onValueChange: changed }), target = flavor === "react" ? r.b : r.a;
    const select = target.querySelector("select")!;
    expect(select.value).toBe("marketing"); expect(root.dataset.theme).toBe("marketing"); expect(changed).not.toHaveBeenCalled();
    await interact(() => { select.value = "application"; select.dispatchEvent(new Event("change", { bubbles: true })); });
    expect(changed).toHaveBeenCalledExactlyOnceWith("application"); expect(root.dataset.theme).toBe("application"); expect(root.dataset.colorScheme).toBe("dark"); expect(root.dataset.density).toBe("compact");
    const denied = { getItem: () => { throw new Error("denied"); }, setItem: () => { throw new Error("quota"); } };
    await r.update({ root, storage: denied });
    await interact(() => { select.value = "marketing"; select.dispatchEvent(new Event("change", { bubbles: true })); });
    expect(select.value).toBe("marketing"); expect(root.dataset.theme).toBe("marketing");
  });
  it("ignores stale/disabled stored options, applies new scopes and resets the native selection", async () => {
    const root = document.createElement("div"), persistence = storage({ [appearanceStorageKey("theme")]: "blocked" });
    const changed = vi.fn(), r = await render("AppearanceSwitch", { root, storage: persistence, defaultValue: "application", onValueChange: changed }), target = flavor === "react" ? r.b : r.a;
    const select = target.querySelector("select")!, form = target.querySelector("form")!; expect(select.value).toBe("application");
    await interact(() => { select.value = "marketing"; select.dispatchEvent(new Event("change", { bubbles: true })); });
    expect(new FormData(form).get("appearance")).toBe("marketing"); await reset(form); expect(select.value).toBe("application"); expect(root.dataset.theme).toBe("application"); expect(changed).toHaveBeenCalledTimes(1);
    await r.update({ axis: "density", options: [{ value: "tiny", label: "Tiny" }, { value: "spacious", label: "Spacious" }], defaultValue: "application", root, storage: persistence });
    expect(select.value).toBe("tiny"); expect(root.dataset.density).toBe("tiny"); expect(root.dataset.theme).toBe("application");
  });
});
for (const [name, props, selector] of [
  ["Avatar", { name: "Ada", fallback: "AL" }, "span"],
  ["ToggleGroup", { legend: "Scope", options, defaultValue: "unread" }, "fieldset"],
  ["AppearanceSwitch", { label: "Theme", axis: "theme", options: appearanceOptions, defaultValue: "marketing", root: document.createElement("div"), storage: storage() }, "select"],
] as const) it(`hydrates ${name} without replacing the native element`, async () => {
  const target = document.createElement("div"), ref = createRef<HTMLElement>(), errors: unknown[] = [];
  document.body.append(target); const element = h(react[name] as ComponentType<any>, { ...props, ref });
  target.innerHTML = renderToString(element); const original = target.querySelector(selector);
  let root!: ReturnType<typeof hydrateRoot>; await act(() => { root = hydrateRoot(target, element, { onRecoverableError: error => errors.push(error) }); });
  try { expect(target.querySelector(selector)).toBe(original); expect(ref.current).toBe(original); expect(errors).toEqual([]); }
  finally { await act(() => root.unmount()); target.remove(); }
});
it("keeps controlled React selection and appearance caller-owned until acknowledged", async () => {
  for (const name of ["ToggleGroup", "AppearanceSwitch"] as const) {
    const target = document.createElement("div"), root = createRoot(target), changed = vi.fn(), appearanceRoot = document.createElement("div"); document.body.append(target);
    const props = { ...base[name], value: name === "ToggleGroup" ? "all" : "application", onValueChange: changed, ...(name === "AppearanceSwitch" ? { root: appearanceRoot, storage: storage({ [appearanceStorageKey("theme")]: "marketing" }) } : {}) };
    const element = () => h("form", null, h(react[name] as ComponentType<any>, props)); await act(() => root.render(element()));
    try {
      if (name === "ToggleGroup") { await interact(() => target.querySelectorAll("button")[3]!.click()); expect(target.querySelector('[aria-pressed="true"]')?.textContent).toBe("All"); }
      else { const select = target.querySelector("select")!; await interact(() => { select.value = "marketing"; select.dispatchEvent(new Event("change", { bubbles: true })); }); expect(select.value).toBe("application"); expect(appearanceRoot.dataset.theme).toBe("application"); }
      expect(changed).toHaveBeenCalledTimes(1); await reset(target.querySelector("form")!);
      props.value = changed.mock.calls[0]![0]; await act(() => root.render(element()));
      if (name === "ToggleGroup") expect(target.querySelector('[aria-pressed="true"]')?.textContent).toBe("Unread");
      else expect(appearanceRoot.dataset.theme).toBe("marketing");
    } finally { await act(() => root.unmount()); target.remove(); }
  }
});

it("adopts an image that already failed before React hydration, then retries replacement sources", async () => {
  const target = document.createElement("div"); document.body.append(target);
  const props = { name: "Workspace", fallback: "WS", src: "/cached-failure.png" };
  target.innerHTML = renderToString(h(react.Avatar, props));
  const image = target.querySelector("img")!;
  Object.defineProperty(image, "complete", { value: true }); Object.defineProperty(image, "naturalWidth", { value: 0 });
  let root!: ReturnType<typeof hydrateRoot>; await act(() => { root = hydrateRoot(target, h(react.Avatar, props)); });
  try {
    expect(target.querySelector("img")).toBeNull(); expect(target.textContent).toBe("WS");
    await act(() => root.render(h(react.Avatar, { ...props, src: "/replacement.png" })));
    expect(target.querySelector("img")?.getAttribute("src")).toBe("/replacement.png");
    await act(() => root.render(h(react.Avatar, props)));
    expect(target.querySelector("img")?.getAttribute("src")).toBe("/cached-failure.png");
  } finally { await act(() => root.unmount()); target.remove(); }
});
