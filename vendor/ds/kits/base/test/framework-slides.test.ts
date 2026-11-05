import { mount, unmount, flushSync } from "svelte";
import { act, createElement as h, createRef, type ComponentType } from "react";
import { createRoot, hydrateRoot } from "react-dom/client";
import { renderToString } from "react-dom/server";
import { expect, it } from "vitest";
import * as react from "../react/src";
import Fixture from "./fixtures/FrameworkSlides.svelte";
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
type Name = "SlideFrame" | "TitleSlide" | "SectionSlide" | "TwoColumnSlide" | "FigureSlide" | "QuoteSlide" | "BulletsSlide" | "Deck";
type Scenario = { name: Name; options: Record<string, unknown> };
const link = () => h("a", { href: "/report" }, "Read report");
function props({ name, options }: Scenario) {
  const common = { ...options, className: "consumer-extra", "data-consumer": "forwarded" };
  if (name === "SlideFrame") return { ...common, children: link() };
  if (name === "Deck") return { ...common, children: h(react.SlideFrame, { label: "Nested slide" }, link()) };
  if (name === "TwoColumnSlide") return { ...common, start: [h("h3", { key: "h" }, "Before"), h("p", { key: "p" }, "Manual review")], end: [h("h3", { key: "h" }, "After"), h("a", { key: "a", href: "/report" }, "Read report")] };
  if (name === "FigureSlide") return { ...common, figure: h("table", null, h("caption", null, "Replicas"), h("tbody", null, h("tr", null, h("th", { scope: "row" }, "api-01"), h("td", null, "Ready")))) };
  return common;
}
async function render(scenario: Scenario) {
  const a = document.createElement("div"); const b = document.createElement("div"); document.body.append(a, b);
  const native = (options: Record<string, unknown>) => ({ ...options, class: "consumer-extra", "data-consumer": "forwarded" });
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
    text: element.children.length ? undefined : element.textContent?.trim() }));
}
const minimal: Scenario[] = [
  { name: "SlideFrame", options: { label: "Overview" } },
  { name: "TitleSlide", options: { title: "Collection review" } },
  { name: "SectionSlide", options: { title: "Coverage" } },
  { name: "TwoColumnSlide", options: { title: "Before and after" } },
  { name: "FigureSlide", options: { title: "Replica health" } },
  { name: "QuoteSlide", options: { quote: "Keep the critical path clear.", attribution: "Operations" } },
  { name: "BulletsSlide", options: { title: "Next steps", items: ["Review", "Ship", "Review"] } },
  { name: "Deck", options: { label: "Collection review" } },
];
const scenarios = minimal.flatMap(scenario => [scenario, ...["background", "raised"].map(surface => ({ ...scenario, options: { ...scenario.options, surface, label: "Custom accessible name", number: 0, total: 7, eyebrow: "Monthly review", level: 3, subtitle: "Ready for review", presenter: "Operations team", summary: "What changed", caption: "Snapshot at 12:00 UTC", source: "Team review" } }))]);
// Extra options above are native forwarded attributes on layouts that do not own them.
for (const scenario of scenarios) it(`matches ${scenario.name} content, naming and frame recipes ${scenario.options.surface ?? "minimal"}`, async () => {
  const r = await render(scenario);
  try { expect(snapshot(r.b)).toEqual(snapshot(r.a)); } finally { await r.cleanup(); }
});
for (const framework of ["svelte", "react"] as const) {
  it(`${framework}: updates frame naming, counts and surfaces without changing native content`, async () => {
    const r = await render(minimal[0]!);
    try {
      const target = framework === "react" ? r.b : r.a;
      const link = target.querySelector<HTMLAnchorElement>("a")!; link.focus(); expect(document.activeElement).toBe(link);
      await r.update({ label: "Updated overview", number: 2, total: 7, surface: "raised" });
      const frame = target.querySelector('[data-slide-frame]')!;
      expect(frame.getAttribute("aria-label")).toBe("Updated overview");
      expect(frame.getAttribute("aria-roledescription")).toBe("slide");
      expect(frame.getAttribute("data-slide-surface")).toBe("raised");
      expect(target.querySelector(".sr-only")?.textContent).toBe("Slide 2 of 7");
      expect(target.querySelector("a")).toBe(link);
      await r.update({ label: "Final overview", number: 3 });
      expect(target.querySelector(".sr-only")?.textContent).toBe("Slide 3");
      await r.update({ label: "Unnumbered" });
      expect(target.querySelector("[data-slide-number-label]")).toBeNull();
    } finally { await r.cleanup(); }
  });
  it(`${framework}: keeps heading outline separate from appearance and updates ordered points`, async () => {
    const r = await render(minimal.find(s => s.name === "BulletsSlide")!);
    try {
      const target = framework === "react" ? r.b : r.a;
      await r.update({ title: "Follow-up", label: "Actions", level: 3, items: [] });
      expect(target.querySelector("h3")?.textContent).toBe("Follow-up");
      expect(target.querySelector("h3")?.getAttribute("data-type-role")).toBe("title");
      expect(target.querySelectorAll("li")).toHaveLength(0);
      await r.update({ title: "Follow-up", items: ["Ship", "Monitor"] });
      expect([...target.querySelectorAll("li")].map(li => li.textContent)).toEqual(["Ship", "Monitor"]);
    } finally { await r.cleanup(); }
  });
}
for (const scenario of minimal) it(`supports native refs and stable SSR hydration for ${scenario.name}`, async () => {
  const target = document.createElement("div"); document.body.append(target);
  const ref = createRef<HTMLElement>();
  const component = h(react[scenario.name] as ComponentType<any>, { ...props(scenario), ref });
  target.innerHTML = renderToString(component);
  const original = target.firstElementChild;
  const errors: unknown[] = [];
  let root: ReturnType<typeof hydrateRoot>;
  await act(() => { root = hydrateRoot(target, component, { onRecoverableError: error => errors.push(error) }); });
  try { expect(ref.current).toBe(original); expect(target.firstElementChild).toBe(original); expect(errors).toEqual([]); }
  finally { await act(() => root!.unmount()); target.remove(); }
});
it("escapes supplied titles and quotations in React SSR", () => {
  const html = renderToString(h(react.QuoteSlide, { quote: '<script>alert("x")</script>', attribution: "Operations" }));
  expect(html).not.toContain("<script>"); expect(html).toContain("&lt;script&gt;");
});
