import { mount, unmount, flushSync, tick } from "svelte";
import { act, createElement as h, createRef, type ComponentType } from "react";
import { createRoot, hydrateRoot } from "react-dom/client";
import { renderToString } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";
import { InlineEdit, Reveal } from "../react/src";
import Fixture from "./fixtures/FrameworkEditReveal.svelte";
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
type Name = "InlineEdit" | "Reveal";
async function render(name: Name, options: Record<string, any> = {}) {
  const a = document.createElement("div"), b = document.createElement("div"); document.body.append(a, b);
  const changed = { svelte: vi.fn(), react: vi.fn() }, editingChanged = { svelte: vi.fn(), react: vi.fn() };
  const base = name === "InlineEdit" ? { label: "Project name", value: "Original", ...options } : options;
  const svelte = mount(Fixture, { target: a, props: { name, options: base, changed: changed.svelte, editingChanged: editingChanged.svelte } }); flushSync();
  const root = createRoot(b); let current = base;
  const element = () => {
    const { class: className, maxlength, onconfirm, ...native } = current;
    return name === "Reveal" ? h(Reveal, { ...native, className }, h("p", null, "Review details")) : h(InlineEdit, {
      ...native, label: native.label, value: native.value, className, maxLength: maxlength, onConfirm: onconfirm,
      onValueChange: value => { changed.react(value); current = { ...current, value }; root.render(element()); },
      onEditingChange: editing => { editingChanged.react(editing); if (current.editing !== undefined) { current = { ...current, editing }; root.render(element()); } },
    });
  };
  await act(() => root.render(element()));
  return { a, b, changed, editingChanged,
    update: async (next: Record<string, any>) => { current = name === "InlineEdit" ? { label: "Project name", value: "Original", ...next } : next; flushSync(() => svelte.update(current)); await act(() => root.render(element())); },
    cleanup: async () => { await act(() => root.unmount()); await unmount(svelte); a.remove(); b.remove(); } };
}
async function interact(fn: () => void | Promise<void>) { await act(async () => { await fn(); flushSync(); await tick(); }); }
async function input(field: HTMLInputElement, value: string) { await interact(() => { Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(field, value); field.dispatchEvent(new Event("input", { bubbles: true })); }); }
async function submit(target: HTMLElement) { await interact(() => { target.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true })); }); }
function snapshot(target: HTMLElement) {
  const ids = [...target.querySelectorAll("[id]")].map(element => element.id);
  return [...target.querySelectorAll("*")].map(element => ({ tag: element.tagName, classes: [...element.classList].sort(),
    attributes: Object.fromEntries([...element.attributes].filter(a => !["class", "value", "style"].includes(a.name) && !a.name.startsWith("data-svelte"))
      .map(a => [a.name, ["id", "aria-labelledby", "aria-describedby"].includes(a.name) ? `id-${ids.indexOf(a.value)}` : a.name.startsWith("data-") && a.value === "true" ? "" : a.value]).sort()),
    style: element.getAttribute("style")?.replace(/\s+/g, ""), value: element instanceof HTMLInputElement ? element.value : undefined,
    text: element.children.length ? undefined : element.textContent?.trim() }));
}
const originalAnimate = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "animate");
afterEach(() => {
  vi.unstubAllGlobals();
  if (originalAnimate) Object.defineProperty(HTMLElement.prototype, "animate", originalAnimate);
  else Reflect.deleteProperty(HTMLElement.prototype, "animate");
});
for (const [name, options] of [
  ["InlineEdit", {}], ["InlineEdit", { value: "", emptyLabel: "No name" }], ["InlineEdit", { disabled: true }],
  ["InlineEdit", { defaultEditing: true, maxlength: 32 }], ["InlineEdit", { editing: true, value: "Initial draft", required: false }],
  ["Reveal", {}], ["Reveal", { direction: "left", delay: 120, debug: true, class: "consumer-reveal" }],
] as [Name, Record<string, any>][]) it(`matches ${name} semantics, appearance and initial state ${JSON.stringify(options)}`, async () => {
  const r = await render(name, options); try { expect(snapshot(r.b)).toEqual(snapshot(r.a)); } finally { await r.cleanup(); }
});
for (const flavor of ["svelte", "react"] as const) describe(`${flavor} inline draft transaction`, () => {
  it("keeps drafts private, cancels with Escape and commits once before returning focus", async () => {
    const confirm = vi.fn().mockResolvedValue(undefined), r = await render("InlineEdit", { onconfirm: confirm });
    try {
      const target = flavor === "react" ? r.b : r.a;
      await interact(() => target.querySelector<HTMLButtonElement>("button")!.click());
      let field = target.querySelector("input")!; expect(document.activeElement).toBe(field); expect(field.value).toBe("Original");
      await input(field, "Discarded"); expect(r.changed[flavor]).not.toHaveBeenCalled();
      await interact(() => field.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true })));
      expect(document.activeElement).toBe(target.querySelector("button"));
      await interact(() => target.querySelector<HTMLButtonElement>("button")!.click()); field = target.querySelector("input")!; expect(field.value).toBe("Original");
      await input(field, "Accepted"); await submit(target);
      expect(confirm).toHaveBeenCalledExactlyOnceWith("Accepted"); expect(r.changed[flavor]).toHaveBeenCalledExactlyOnceWith("Accepted");
      expect(target.textContent).toContain("Accepted"); expect(target.querySelector("input")).toBeNull(); expect(document.activeElement).toBe(target.querySelector("button"));
      expect(r.editingChanged[flavor].mock.calls.map(call => call[0])).toEqual([true, false, true, false]);
    } finally { await r.cleanup(); }
  });
  it("retains invalid/rejected drafts, names the error, recovers focus and retries", async () => {
    const confirm = vi.fn().mockRejectedValueOnce(new Error("offline")).mockResolvedValue(undefined);
    const r = await render("InlineEdit", { defaultEditing: true, onconfirm: confirm, validate: (value: string) => value === "Duplicate" ? "Already used." : undefined });
    try {
      const target = flavor === "react" ? r.b : r.a, field = target.querySelector("input")!;
      for (const [draft, error] of [["   ", "Enter a value."], ["Duplicate", "Already used."]]) {
        await input(field, draft!); await submit(target); expect(confirm).not.toHaveBeenCalled();
        const alert = target.querySelector('[role="alert"]')!; expect(alert.textContent).toBe(error); expect(field.getAttribute("aria-describedby")).toBe(alert.id);
      }
      await input(field, "Retry me"); await submit(target); expect(field.value).toBe("Retry me"); expect(document.activeElement).toBe(field);
      expect(field.disabled).toBe(false); expect(target.querySelector('[role="alert"]')!.textContent).toBe("Could not save. Try again."); expect(r.changed[flavor]).not.toHaveBeenCalled();
      await submit(target); expect(r.changed[flavor]).toHaveBeenCalledExactlyOnceWith("Retry me");
    } finally { await r.cleanup(); }
  });
  it("blocks duplicate saves and ignores confirmations after a newer value or closed session", async () => {
    const completions: (() => void)[] = [], confirm = vi.fn(() => new Promise<void>(resolve => completions.push(resolve)));
    const r = await render("InlineEdit", { defaultEditing: true, onconfirm: confirm });
    try {
      const target = flavor === "react" ? r.b : r.a, field = target.querySelector("input")!;
      await input(field, "Pending");
      await interact(() => { const form = target.querySelector("form")!; form.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true })); form.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true })); });
      expect(confirm).toHaveBeenCalledTimes(1); expect(field.disabled).toBe(true);
      await r.update({ editing: true, value: "Newer committed value", onconfirm: confirm }); expect(field.value).toBe("Newer committed value");
      await interact(() => completions.shift()!()); expect(r.changed[flavor]).not.toHaveBeenCalled();
      await input(field, "Another pending draft"); await submit(target); await r.update({ editing: false, value: "Newer committed value", onconfirm: confirm });
      await interact(() => completions.shift()!()); expect(r.changed[flavor]).not.toHaveBeenCalled(); expect(target.querySelector("input")).toBeNull();
    } finally { await r.cleanup(); }
  });
  it("ignores confirmations after unmount and keeps IME Enter inside the input", async () => {
    let finish!: () => void; const confirm = vi.fn(() => new Promise<void>(resolve => { finish = resolve; }));
    const r = await render("InlineEdit", { defaultEditing: true, onconfirm: confirm });
    const target = flavor === "react" ? r.b : r.a, field = target.querySelector("input")!;
    const event = new KeyboardEvent("keydown", { key: "Enter", isComposing: true, bubbles: true, cancelable: true });
    await interact(() => field.dispatchEvent(event)); expect(event.defaultPrevented).toBe(true);
    await submit(target); await r.cleanup(); await interact(() => finish()); expect(r.changed[flavor]).not.toHaveBeenCalled();
  });
});
for (const name of ["InlineEdit", "Reveal"] as const) it(`hydrates useful ${name} HTML without replacing its native elements`, async () => {
  const target = document.createElement("div"); document.body.append(target);
  const ref = createRef<HTMLDivElement>(), props = name === "InlineEdit" ? { label: "Project", value: "Initial draft", defaultEditing: true, ref } : { ref };
  const element = h((name === "InlineEdit" ? InlineEdit : Reveal) as ComponentType<any>, props, ...(name === "Reveal" ? [h("p", null, "Review details")] : []));
  target.innerHTML = renderToString(element); const original = target.firstElementChild, errors: unknown[] = [];
  if (name === "InlineEdit") expect(target.querySelector("input")!.value).toBe("Initial draft");
  else { expect(target.textContent).toBe("Review details"); expect(target.firstElementChild!.classList).toContain("opacity-100"); }
  let root: ReturnType<typeof hydrateRoot>;
  await act(() => { root = hydrateRoot(target, element, { onRecoverableError: error => errors.push(error) }); });
  try { expect(target.firstElementChild).toBe(original); expect(ref.current).toBe(original); expect(errors).toEqual([]); }
  finally { await act(() => root!.unmount()); target.remove(); }
});

function motionPreference(legacy = false) {
  let matches = false;
  const listeners = new Set<() => void>();
  const add = (_name: string, listener: () => void) => listeners.add(listener), remove = (_name: string, listener: () => void) => listeners.delete(listener);
  vi.stubGlobal("matchMedia", () => ({ get matches() { return matches; }, ...(legacy ? { addListener: (listener: () => void) => listeners.add(listener), removeListener: (listener: () => void) => listeners.delete(listener) } : { addEventListener: add, removeEventListener: remove }) }));
  return { listeners, set: async (next: boolean) => { await interact(() => { matches = next; listeners.forEach(listener => listener()); }); } };
}
for (const flavor of ["svelte", "react"] as const) describe(`${flavor} native entry motion lifecycle`, () => {
  it("enters on intersection, follows live reduced motion and disconnects observers/listeners", async () => {
    vi.stubGlobal("CSS", { supports: () => false }); const preference = motionPreference();
    const observers: { target?: Element; callback: (entries: { isIntersecting: boolean }[]) => void; disconnect: ReturnType<typeof vi.fn> }[] = [];
    vi.stubGlobal("IntersectionObserver", class { item: typeof observers[number]; constructor(callback: typeof observers[number]["callback"]) { this.item = { callback, disconnect: vi.fn() }; observers.push(this.item); } observe(target: Element) { this.item.target = target; } disconnect() { this.item.disconnect(); } });
    const r = await render("Reveal", { direction: "right", delay: 180 });
    try {
      const element = (flavor === "react" ? r.b : r.a).querySelector<HTMLElement>("[data-reveal]")!, observer = observers.find(item => item.target === element)!;
      expect(element.hasAttribute("data-reveal-entered")).toBe(false); expect(element.style.getPropertyValue("--reveal-delay")).toBe("180ms");
      await interact(() => observer.callback([{ isIntersecting: false }])); expect(element.hasAttribute("data-reveal-entered")).toBe(false);
      await interact(() => observer.callback([{ isIntersecting: true }])); expect(element.hasAttribute("data-reveal-entered")).toBe(true); expect(observer.disconnect).toHaveBeenCalledTimes(1);
      await preference.set(true); expect(element.getAttribute("data-reveal-motion")).toBe("fade"); expect(element.style.getPropertyValue("--reveal-x")).toBe("0px");
      await preference.set(false); expect(element.style.getPropertyValue("--reveal-x")).toBe(""); expect(element.hasAttribute("data-reveal-entered")).toBe(true);
      expect(observers).toHaveLength(2);
    } finally { await r.cleanup(); }
    expect(preference.listeners.size).toBe(0); expect(observers.every(observer => observer.disconnect.mock.calls.length === 1)).toBe(true);
  });
  it("cancels native timelines on option/preference changes and cleanup", async () => {
    vi.stubGlobal("CSS", { supports: () => true }); vi.stubGlobal("ViewTimeline", class { constructor(public options: { subject: Element }) {} });
    const preference = motionPreference();
    const animations: { target: HTMLElement; frames: Keyframe[]; cancel: ReturnType<typeof vi.fn> }[] = [];
    Object.defineProperty(HTMLElement.prototype, "animate", { configurable: true, value: function(this: HTMLElement, frames: Keyframe[]) { const item = { target: this, frames, cancel: vi.fn() }; animations.push(item); return item; } });
    const r = await render("Reveal", { direction: "down" });
    try {
      const element = (flavor === "react" ? r.b : r.a).querySelector<HTMLElement>("[data-reveal]")!, animation = animations.find(item => item.target === element)!;
      expect(element.getAttribute("data-reveal-path")).toBe("css"); expect(animation.frames[0]!.transform).toContain("var(--reddb-spatial-inset-lg)");
      await r.update({ direction: "left" }); expect(animation.cancel).toHaveBeenCalledTimes(1); expect(animations).toHaveLength(4);
      await preference.set(true); expect(element.hasAttribute("data-reveal-entered")).toBe(true); expect(element.getAttribute("data-reveal-motion")).toBe("fade");
      await preference.set(false); expect(animations).toHaveLength(4);
    } finally { await r.cleanup(); }
    expect(preference.listeners.size).toBe(0); expect(animations.every(animation => animation.cancel.mock.calls.length === 1)).toBe(true);
  });
  it("settles visible content when motion APIs fail or are unavailable, including legacy media listeners", async () => {
    vi.stubGlobal("CSS", { supports: () => true }); vi.stubGlobal("ViewTimeline", class {});
    Object.defineProperty(HTMLElement.prototype, "animate", { configurable: true, value: () => { throw new Error("unsupported timeline"); } });
    const preference = motionPreference(true), r = await render("Reveal");
    try {
      const element = (flavor === "react" ? r.b : r.a).querySelector<HTMLElement>("[data-reveal]")!;
      expect(element.hasAttribute("data-reveal-entered")).toBe(true); await preference.set(true); expect(element.getAttribute("data-reveal-motion")).toBe("fade");
    } finally { await r.cleanup(); }
    expect(preference.listeners.size).toBe(0);
  });
});
it("keeps caller-owned React values and editing state until acknowledged", async () => {
  const target = document.createElement("div"); document.body.append(target); const root = createRoot(target);
  const changed = vi.fn(), editing = vi.fn();
  await act(() => root.render(h(InlineEdit, { label: "Project", value: "Original", editing: true, onValueChange: changed, onEditingChange: editing })));
  try {
    await input(target.querySelector("input")!, "Proposed"); await submit(target);
    expect(changed).toHaveBeenCalledExactlyOnceWith("Proposed"); expect(editing).toHaveBeenCalledExactlyOnceWith(false); expect(target.querySelector("input")).not.toBeNull();
    await act(() => root.render(h(InlineEdit, { label: "Project", value: "Original", editing: false })));
    expect(target.textContent).toContain("Original"); expect(target.textContent).not.toContain("Proposed");
  } finally { await act(() => root.unmount()); target.remove(); }
});
it("refuses unusable motion delays during SSR", () => {
  for (const delay of [-1, Infinity, NaN]) expect(() => renderToString(h(Reveal, { delay }, "Details"))).toThrow("Reveal: delay");
});
