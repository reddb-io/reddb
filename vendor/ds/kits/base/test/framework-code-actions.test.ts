import { mount, unmount, flushSync, tick } from "svelte";
import { act, createElement as h, createRef, type ComponentType } from "react";
import { createRoot, hydrateRoot } from "react-dom/client";
import { renderToString } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";
import * as react from "../react/src";
import Fixture from "./fixtures/FrameworkCodeActions.svelte";
import type { CodeTokenizer } from "../src/code-block.variants";
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
const source = 'const request = "<sample>";\n\treturn 42;\n';
const grammar: CodeTokenizer = { tokenize: code => [...code.matchAll(/const|return|\d+|"[^"]*"|[\s\S]/g)].map(([value]) => ({ value, kind: value === "const" || value === "return" ? "keyword" : /^\d+$/.test(value) ? "number" : value.startsWith('"') ? "string" : "text" })) };
type Name = "Swap" | "CodeBlock";
async function render(name: Name, options: Record<string, any> = {}) {
  const a = document.createElement("div"), b = document.createElement("div"); document.body.append(a, b);
  const events = { svelte: [] as unknown[], react: [] as unknown[] };
  const changed = { svelte: vi.fn(next => events.svelte.push(next)), react: vi.fn(next => events.react.push(next)) };
  const clicked = { svelte: () => events.svelte.push("click"), react: () => events.react.push("click") };
  const base = { label: "Pin request", code: source, ...options };
  const svelte = mount(Fixture, { target: a, props: { name, options: base, clicked: clicked.svelte, changed: changed.svelte } }); flushSync();
  const root = createRoot(b);
  const element = (next: Record<string, any>) => {
    const { class: className, faces, ...props } = next;
    return h(react[name] as ComponentType<any>, { ...props, className, ...(name === "Swap" ? { onClick: clicked.react, onSwappedChange: changed.react } : {}), ...(faces ? { off: [h("span", { key: "icon", "aria-hidden": true }, "◇"), " Pin"], on: [h("span", { key: "icon", "aria-hidden": true }, "◆"), " Pin"] } : {}) });
  };
  await act(() => root.render(element(base)));
  return { a, b, events, changed, update: async (next: Record<string, any>) => { const props = { label: "Pin request", code: source, ...next }; flushSync(() => svelte.update(props)); await act(() => root.render(element(props))); }, cleanup: async () => { await act(() => root.unmount()); await unmount(svelte); a.remove(); b.remove(); } };
}
async function interact(fn: () => void | Promise<void>) { await act(async () => { await fn(); flushSync(); await tick(); }); }
function snapshot(target: HTMLElement) {
  return [...target.querySelectorAll("*")].map(element => ({ tag: element.tagName, classes: [...element.classList].sort(),
    attributes: Object.fromEntries([...element.attributes].filter(a => a.name !== "class" && !a.name.startsWith("data-svelte"))
      .map(a => [a.name, a.name.startsWith("data-") && a.value === "true" ? "" : a.value]).sort()),
    text: element.children.length ? undefined : element.textContent?.trim() }));
}
afterEach(() => { Reflect.deleteProperty(navigator, "clipboard"); vi.unstubAllGlobals(); });
for (const [name, options] of [
  ["Swap", {}], ["Swap", { defaultSwapped: true }], ["Swap", { swapped: true, faces: true }], ["Swap", { faces: true, disabled: true, class: "shrink-0" }],
  ["CodeBlock", {}], ["CodeBlock", { language: "TypeScript", grammar }], ["CodeBlock", { grammar: { tokenize: () => [{ kind: "keyword", value: "lost whitespace" }] } }],
  ["CodeBlock", { code: "", copyLabel: "Copy query", copiedLabel: "Query copied", copyErrorLabel: "Retry copy", regionLabel: "Request query" }],
] as [Name, Record<string, any>][]) it(`matches ${name} native semantics and appearance ${JSON.stringify(options)}`, async () => {
  const r = await render(name, options); try { expect(snapshot(r.b)).toEqual(snapshot(r.a)); } finally { await r.cleanup(); }
});
for (const flavor of ["svelte", "react"] as const) describe(`${flavor} code and state actions`, () => {
  it("swaps only one face, preserves its name, and reports click before the next state", async () => {
    const r = await render("Swap", { faces: true });
    try {
      const target = flavor === "react" ? r.b : r.a, button = target.querySelector("button")!;
      expect(button.type).toBe("button"); expect(button.getAttribute("aria-pressed")).toBe("false");
      await interact(() => button.click());
      expect(button.getAttribute("aria-pressed")).toBe("true"); expect(button.textContent).toContain("◆"); expect(button.textContent).not.toContain("◇");
      expect(button.getAttribute("aria-label")).toBe("Pin request"); expect(r.events[flavor]).toEqual(["click", true]);
      await interact(() => button.click()); expect(r.events[flavor]).toEqual(["click", true, "click", false]);
      await r.update({ faces: true, disabled: true }); await interact(() => button.click()); expect(r.events[flavor]).toHaveLength(4);
    } finally { await r.cleanup(); }
  });
  it("keeps the initial default face through unrelated updates", async () => {
    const r = await render("Swap", { defaultSwapped: true });
    try {
      const button = (flavor === "react" ? r.b : r.a).querySelector("button")!;
      await interact(() => button.click()); expect(button.getAttribute("aria-pressed")).toBe("false");
      await r.update({ defaultSwapped: true, title: "Inspect request" }); expect(button.getAttribute("aria-pressed")).toBe("false");
    } finally { await r.cleanup(); }
  });
  it("copies exact source, reports failure, permits retry and resets feedback on new source", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined); Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    const r = await render("CodeBlock", { grammar });
    try {
      const target = flavor === "react" ? r.b : r.a, button = target.querySelector("button")!;
      expect(target.querySelector("code")!.textContent).toBe(source);
      await interact(() => button.click()); expect(writeText).toHaveBeenCalledExactlyOnceWith(source); expect(button.textContent?.trim()).toBe("Copied");
      await r.update({ code: "select 1;" }); expect(button.textContent?.trim()).toBe("Copy code");
      writeText.mockRejectedValueOnce(new Error("denied")); await interact(() => button.click()); expect(button.textContent?.trim()).toBe("Copy failed. Try again.");
      await interact(() => button.click()); expect(button.textContent?.trim()).toBe("Copied"); expect(writeText).toHaveBeenLastCalledWith("select 1;");
      Reflect.deleteProperty(navigator, "clipboard"); await interact(() => button.click()); expect(button.textContent?.trim()).toBe("Copy failed. Try again.");
    } finally { await r.cleanup(); }
  });
  it("ignores superseded clipboard completions and completions after source changes or unmount", async () => {
    const completions: (() => void)[] = [];
    const writeText = vi.fn(() => new Promise<void>(resolve => completions.push(resolve)));
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    const r = await render("CodeBlock");
    const button = (flavor === "react" ? r.b : r.a).querySelector("button")!;
    await interact(() => button.click()); await r.update({ code: "new source" }); await interact(() => completions.shift()!()); expect(button.textContent?.trim()).toBe("Copy code");
    await interact(() => button.click()); await interact(() => button.click()); await interact(() => completions.shift()!()); expect(button.textContent?.trim()).toBe("Copy code");
    await interact(() => completions.shift()!()); expect(button.textContent?.trim()).toBe("Copied");
    await interact(() => button.click()); await r.cleanup(); await interact(() => completions.shift()!());
  });
  it("measures overflowing source changes, gives the region a name and disposes observers", async () => {
    const disconnects: ReturnType<typeof vi.fn>[] = [];
    const callbacks: (() => void)[] = [];
    vi.stubGlobal("ResizeObserver", class { disconnect = vi.fn(); constructor(callback: () => void) { disconnects.push(this.disconnect); callbacks.push(callback); } observe() {} });
    const r = await render("CodeBlock", { language: "SQL" });
    const pre = (flavor === "react" ? r.b : r.a).querySelector("pre")!;
    try {
      expect(pre.hasAttribute("tabindex")).toBe(false);
      Object.defineProperty(pre, "scrollWidth", { configurable: true, value: 900 }); Object.defineProperty(pre, "clientWidth", { configurable: true, value: 300 });
      await interact(() => callbacks.forEach(callback => callback()));
      expect(pre.tabIndex).toBe(0); expect(pre.getAttribute("role")).toBe("region"); expect(pre.getAttribute("aria-label")).toBe("SQL code");
      await r.update({ code: "select 1;", regionLabel: "Query" }); expect(pre.getAttribute("aria-label")).toBe("Query");
      Object.defineProperty(pre, "scrollWidth", { configurable: true, value: 100 });
      await r.update({ code: "1" }); expect(pre.hasAttribute("tabindex")).toBe(false); expect(pre.hasAttribute("aria-label")).toBe(false);
    } finally { await r.cleanup(); }
    expect(disconnects.every(disconnect => disconnect.mock.calls.length === 1)).toBe(true);
  });
});
it("keeps caller-owned React faces until the caller accepts the requested change", async () => {
  const r = await render("Swap", { swapped: false });
  try {
    const button = r.b.querySelector("button")!; await interact(() => button.click());
    expect(button.getAttribute("aria-pressed")).toBe("false"); expect(r.events.react).toEqual(["click", true]);
    await r.update({ swapped: true }); expect(button.getAttribute("aria-pressed")).toBe("true");
  } finally { await r.cleanup(); }
});
for (const name of ["Swap", "CodeBlock"] as const) it(`hydrates useful ${name} SSR with native refs and unchanged DOM`, async () => {
  const target = document.createElement("div"); document.body.append(target);
  const ref = createRef<any>(), component = h(react[name] as ComponentType<any>, { label: "Pin request", code: source, grammar, ref });
  target.innerHTML = renderToString(component); const original = target.firstElementChild, errors: unknown[] = [];
  if (name === "CodeBlock") { expect(target.querySelector("code")!.textContent).toBe(source); expect(target.querySelector('[data-code-token="keyword"]')).not.toBeNull(); expect(target.querySelector("pre")!.hasAttribute("tabindex")).toBe(false); }
  let root: ReturnType<typeof hydrateRoot>;
  await act(() => { root = hydrateRoot(target, component, { onRecoverableError: error => errors.push(error) }); });
  try { expect(target.firstElementChild).toBe(original); expect(ref.current).toBe(original); expect(errors).toEqual([]); }
  finally { await act(() => root!.unmount()); target.remove(); }
});
