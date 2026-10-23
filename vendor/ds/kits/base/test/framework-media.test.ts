import { mount, unmount, flushSync } from "svelte";
import { act, createElement as h, forwardRef, type ComponentType } from "react";
import { createRoot } from "react-dom/client";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import * as react from "../react/src";
import { ICON_COLORS, ICON_SIZES } from "../src/core/icon";
import { LOGO_LAYOUTS, LOGO_SURFACES, shipsMark } from "../src/logo.marks";
import { CARD_MEDIA_RATIOS, CARD_MEDIA_SPANS, CARD_TONES } from "../src/card.variants";
import { IMAGE_FITS, IMAGE_POSITIONS } from "../src/image.variants";
import Fixture from "./fixtures/FrameworkMedia.svelte";
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
const Glyph = forwardRef<SVGSVGElement, react.IconGlyphProps>(({ size, color, strokeWidth, ...rest }, ref) => h("svg", { ...rest, ref, "data-test-glyph": true, width: size, height: size, color, stroke: color, strokeWidth, viewBox: "0 0 24 24" }));
const image = { src: "/photo.webp", alt: "São Paulo region", width: 800, height: 600 };
type Scenario = { name: "Icon" | "Image" | "Logo" | "Card"; options?: Record<string, unknown>; rich?: boolean; glyph?: boolean };
function props({ name, options = {}, rich, glyph }: Scenario) {
  const common = { ...options, className: "consumer-extra", "data-consumer": "forwarded" };
  if (name === "Icon") return { ...common, ...(glyph ? { icon: Glyph } : {}) };
  if (name !== "Card") return common;
  return { ...common, ...(glyph ? { icon: Glyph } : {}), ...(rich ? { header: h("strong", null, "Custom header"), media: h("span", null, "Custom media") } : {}), footer: h("button", { type: "button" }, "Details"), children: h("span", null, "Card content") };
}
async function render(scenario: Scenario) {
  const a = document.createElement("div"); const b = document.createElement("div"); document.body.append(a, b);
  const native = (options: Record<string, unknown> = {}) => ({ ...options, class: "consumer-extra", "data-consumer": "forwarded" });
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
const scenarios: Scenario[] = [
  ...ICON_SIZES.flatMap(size => ICON_COLORS.flatMap(color => [false, true].map(glyph => ({ name: "Icon" as const, options: { icon: "menu", size, color, "aria-label": "Navigation" }, glyph })))),
  ...IMAGE_FITS.flatMap(fit => IMAGE_POSITIONS.flatMap(position => [false, true].map(priority => ({ name: "Image" as const, options: { ...image, fit, position, priority } })))),
  { name: "Image", options: { ...image, alt: "", decorative: true, ratio: 1 } },
  { name: "Image", options: { ...image, priority: true, loading: "lazy", srcset: "/small.webp 400w, /large.webp 800w", sizes: "50vw", sources: [{ srcset: "/photo.avif", type: "image/avif" }, { srcset: "/crop.webp", media: "(min-width: 48rem)", sizes: "30vw" }] } },
  ...LOGO_LAYOUTS.flatMap(layout => LOGO_SURFACES.filter(on => shipsMark(layout, on)).flatMap(on => [false, true].map(link => ({ name: "Logo" as const, options: { layout, on, ...(link ? { href: "/", label: "RedDB home" } : {}), size: 24, loading: "lazy", fetchpriority: "low" } })))),
  ...CARD_TONES.flatMap(tone => ["outline", "plain"].flatMap(variant => [false, true].map(raised => ({ name: "Card" as const, options: { tone, variant, raised, title: "Region", description: "Primary cluster", busy: raised }, glyph: true })))),
  ...CARD_MEDIA_SPANS.flatMap(span => CARD_MEDIA_RATIOS.flatMap(ratio => ["horizontal", "vertical"].map(orientation => ({ name: "Card" as const, options: { image, span, ratio, orientation, priority: true } })))),
  { name: "Card", rich: true, glyph: true, options: { title: "Replaced", description: "Replaced", image, busy: true, busyLabel: "Refreshing region", busyLabelHidden: true } },
];
for (const scenario of scenarios) it(`matches ${scenario.name} semantics, content, geometry and appearance ${JSON.stringify(scenario)}`, async () => {
  const result = await render(scenario);
  try { expect(snapshot(result.b)).toEqual(snapshot(result.a)); } finally { await result.cleanup(); }
});
for (const framework of ["react", "svelte"] as const) describe(`${framework} media behavior`, () => {
  it("updates background work without replacing content or custom sections", async () => {
    const r = await render({ name: "Card", rich: true, options: { title: "Replaced", image } });
    try {
      const target = framework === "react" ? r.b : r.a;
      const content = target.querySelector("[data-card-body] > span");
      expect(target.textContent).not.toContain("Replaced"); expect(target.querySelector("img")).toBeNull();
      await r.update({ title: "Replaced", image, busy: true, busyLabel: "Refreshing" });
      expect(target.querySelector("[data-card-body] > span")).toBe(content);
      expect(target.querySelector('[aria-busy="true"]')).not.toBeNull();
      expect(target.querySelector('[role="status"]')?.textContent).toContain("Refreshing");
      await r.update({ title: "Replaced", image });
      expect(target.querySelector('[role="status"]')).toBeNull();
    } finally { await r.cleanup(); }
  });
  it("follows document Color Scheme changes and respects explicit local surfaces", async () => {
    document.documentElement.setAttribute("data-color-scheme", "light");
    const r = await render({ name: "Logo" });
    try {
      const target = framework === "react" ? r.b : r.a;
      const before = target.querySelector("img")!.getAttribute("src");
      await act(async () => { document.documentElement.setAttribute("data-color-scheme", "dark"); await new Promise(resolve => setTimeout(resolve, 0)); flushSync(); });
      expect(target.firstElementChild?.getAttribute("data-logo-on")).toBe("dark");
      expect(target.querySelector("img")!.getAttribute("src")).not.toBe(before);
      await r.update({ on: "light" });
      expect(target.firstElementChild?.getAttribute("data-logo-on")).toBe("light");
    } finally { await r.cleanup(); document.documentElement.removeAttribute("data-color-scheme"); }
  });
});
it("React refuses unlabelled, unsized or contradictory images and unshipped or illegible Marks during SSR", () => {
  for (const options of [{ src: "/x", alt: "", ratio: 1 }, { src: "/x", alt: "Content" }, { src: "/x", alt: "Content", decorative: true, ratio: 1 }]) {
    expect(() => renderToString(h(react.Image, options as react.ImageProps))).toThrow("Image:");
  }
  expect(() => renderToString(h(react.Logo, { layout: "icon", on: "dark" }))).toThrow("ships no icon Mark");
  expect(() => renderToString(h(react.Logo, { size: 8 }))).toThrow("below the 16px minimum");
});

it("React accepts numeric renderable slots without falling back or leaking them outside sections", () => {
  const html = renderToString(h(react.Card, { title: "Replaced title", image, header: 0, media: 0, footer: 0 }));
  const target = document.createElement("div"); target.innerHTML = html;
  for (const slot of ["header", "media", "footer"]) expect(target.querySelector(`[data-card-${slot}]`)?.textContent).toBe("0");
  expect(target.querySelector("img")).toBeNull(); expect(target.textContent).not.toContain("Replaced title");
});
