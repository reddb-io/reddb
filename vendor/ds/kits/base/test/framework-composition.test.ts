import { mount, unmount, flushSync } from "svelte";
import { act, createElement as h, type ComponentType } from "react";
import { createRoot } from "react-dom/client";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import * as react from "../react/src";
import { TONES } from "../src/tone";
import Fixture from "./fixtures/FrameworkComposition.svelte";
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
type Scenario = { name: "Link" | "Alert" | "Notification" | "EmptyState" | "MediaObject" | "ListContainer" | "Breadcrumbs" | "BrowserMockup" | "WindowMockup" | "PhoneMockup"; options?: Record<string, unknown>; content?: boolean; rich?: boolean };
function props({ name, options = {}, content = true, rich }: Scenario) {
  const { ondismiss, ...native } = options;
  const common = { ...native, ...(ondismiss ? { onDismiss: ondismiss } : {}), className: "consumer-extra", ...(name === "Breadcrumbs" ? {} : { "data-consumer": "forwarded" }) };
  if (name === "Breadcrumbs") return common;
  if (name === "EmptyState") return { ...common, ...(rich ? { media: h("span", { "data-slot": "media" }, "M"), actions: h("button", { type: "button" }, "Create") } : {}) };
  if (name === "MediaObject") return { ...common, media: h("span", { "data-slot": "media" }, "M"), children: h("a", { href: "/detail", "data-slot": "content" }, "Details") };
  if (name === "ListContainer") return { ...common, ...(rich ? { header: h("strong", null, "Custom title"), footer: h("button", { type: "button" }, "Create") } : {}), children: h("ul", null, h("li", null, "Destination")) };
  return { ...common, ...(content ? { children: h("span", { "data-slot": "body" }, "Content") } : {}) };
}
async function render(scenario: Scenario) {
  const a = document.createElement("div"); const b = document.createElement("div"); document.body.append(a, b);
  const native = (options: Record<string, unknown> = {}) => ({ ...options, class: "consumer-extra", ...(scenario.name === "Breadcrumbs" ? {} : { "data-consumer": "forwarded" }) });
  const svelte = mount(Fixture, { target: a, props: { ...scenario, options: native(scenario.options) } }); flushSync();
  const root = createRoot(b);
  await act(() => root.render(h(react[scenario.name] as ComponentType<any>, props(scenario))));
  return { a, b, update: async (options: Record<string, unknown>) => {
    flushSync(() => svelte.update(native(options)));
    await act(() => root.render(h(react[scenario.name] as ComponentType<any>, props({ ...scenario, options }))));
  }, cleanup: async () => { await act(() => root.unmount()); await unmount(svelte); a.remove(); b.remove(); } };
}
function snapshot(target: HTMLElement) {
  return [...target.querySelectorAll("*")].map(element => ({ tag: element.tagName, classes: [...element.classList].sort(),
    attributes: Object.fromEntries([...element.attributes].filter(a => a.name !== "class" && a.name !== "style" && !a.name.startsWith("data-svelte"))
      .map(a => [a.name, a.name.startsWith("data-") && a.value === "true" ? "" : a.value]).sort()),
    style: element.getAttribute("style") ? Object.fromEntries([...((element as HTMLElement).style)].sort().map(n => [n, (element as HTMLElement).style.getPropertyValue(n)])) : undefined,
    text: element.children.length ? undefined : element.textContent?.trim() }));
}
const items = [{ id: "home", label: "Home", href: "/" }, { id: "group", label: "Clusters" }, { id: "current", label: "Production", href: "/production", current: true }];
const scenarios: Scenario[] = [
  { name: "Link", options: { href: "/destination", target: "_blank", rel: "noreferrer", "aria-label": "Destination", download: "report" } },
  ...["Alert", "Notification"].flatMap(name => [undefined, ...TONES].flatMap(tone => [false, true].map(content => ({ name: name as "Alert" | "Notification", content, options: { tone, title: "Workspace updated" } })))),
  { name: "Alert" }, { name: "Alert", content: false },
  ...["sm", "md"].flatMap(size => [false, true].flatMap(bordered => [false, true].map(rich => ({ name: "EmptyState" as const, rich, options: { title: "No requests", description: "Create a request to begin", hint: "red request new", size, bordered } })))),
  { name: "EmptyState", options: { title: "No results" } },
  ...["start", "center", "end"].flatMap(align => ["sm", "md", "lg"].map(gap => ({ name: "MediaObject" as const, options: { align, gap, mediaClass: "media-extra", contentClass: "content-extra" } }))),
  ...["plain", "outline"].flatMap(variant => ["none", "sm", "md"].flatMap(padding => [false, true].map(rich => ({ name: "ListContainer" as const, rich, options: { title: "Destinations", description: "Pinned regions", variant, padding } })))),
  ...[false, true, { origin: "https://console.example.com" }].map(structuredData => ({ name: "Breadcrumbs" as const, options: { items, structuredData, label: "Cluster location" } })),
  { name: "Breadcrumbs", options: { items: [] } },
  { name: "Breadcrumbs", options: { items: [{ id: "unsafe", label: '</script><img src=x onerror="alert(1)">&\u2028', href: "/?x=1&y=2" }], structuredData: true } },
  ...["BrowserMockup", "WindowMockup", "PhoneMockup"].flatMap(name => [undefined, 1, 4 / 3, -1, Number.NaN].map(ratio => ({ name: name as "BrowserMockup" | "WindowMockup" | "PhoneMockup", options: { ...(ratio === undefined ? {} : { ratio }), ...(name === "BrowserMockup" ? { address: "https://example.com" } : name === "WindowMockup" ? { title: "Workspace" } : {}) } }))),
];
for (const scenario of scenarios) it(`matches ${scenario.name} content, appearance and semantics ${JSON.stringify(scenario.options ?? {})}`, async () => {
  const r = await render(scenario);
  try { expect(snapshot(r.b)).toEqual(snapshot(r.a)); } finally { await r.cleanup(); }
});
for (const framework of ["svelte", "react"] as const) describe(`${framework} composition behavior`, () => {
  it("updates Notification urgency and text, then dismisses using its native control", async () => {
    let dismissals = 0;
    const r = await render({ name: "Notification", options: { title: "Saved", tone: "success", ondismiss: () => dismissals++ } });
    try {
      const target = framework === "react" ? r.b : r.a;
      expect(target.querySelector('[role="status"]')).not.toBeNull();
      await r.update({ title: "Request failed", tone: "danger", dismissLabel: "Dismiss error", ondismiss: () => dismissals++ });
      expect(target.querySelector('[role="alert"]')?.textContent).toContain("Request failed");
      const button = target.querySelector("button")!; button.focus(); expect(document.activeElement).toBe(button);
      await act(() => { button.click(); flushSync(); });
      expect(target.querySelector("[data-notification]")).toBeNull(); expect(dismissals).toBe(1);
    } finally { await r.cleanup(); }
  });
  it("omits an empty Notification detail region", async () => {
    const r = await render({ name: "Notification", content: false, options: { title: "Saved" } });
    try { expect((framework === "react" ? r.b : r.a).querySelector('[role="status"] > div')).toBeNull(); } finally { await r.cleanup(); }
  });
  it("keeps breadcrumb positions, current page semantics and JSON-LD aligned after updates", async () => {
    const r = await render({ name: "Breadcrumbs", options: { items, structuredData: { origin: "https://example.com" } } });
    try {
      const target = framework === "react" ? r.b : r.a;
      expect(target.querySelector('[aria-current="page"]')?.tagName).toBe("SPAN");
      expect(JSON.parse(target.querySelector("script")!.textContent!).itemListElement[0].item).toBe("https://example.com/");
      await r.update({ items: [{ id: "only", label: "Home", current: true }], structuredData: false });
      expect(target.querySelectorAll("li")).toHaveLength(1); expect(target.querySelector("a")).toBeNull(); expect(target.querySelector("script")).toBeNull();
    } finally { await r.cleanup(); }
  });
});
it("safely emits React breadcrumb JSON-LD in server HTML, preserving original labels", () => {
  const label = '</script><img src=x onerror="alert(1)">';
  const html = renderToString(h(react.Breadcrumbs, { items: [{ id: "unsafe", label, current: true }], structuredData: true }));
  const target = document.createElement("div"); target.innerHTML = html;
  expect(target.querySelectorAll("script")).toHaveLength(1); expect(target.querySelector("img")).toBeNull();
  expect(JSON.parse(target.querySelector("script")!.textContent!).itemListElement[0].name).toBe(label);
});
