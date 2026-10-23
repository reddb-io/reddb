import { mount, unmount, flushSync } from "svelte";
import { act, createElement as h, type ComponentType } from "react";
import { createRoot } from "react-dom/client";
import { describe, expect, it } from "vitest";
import * as react from "../react/src";
import type { FieldsetControlProps } from "../src/core/fieldset";
import { CONTENT_MASK_SHAPES } from "../src/content-mask.variants";
import { SKELETON_SHAPES } from "../src/skeleton.variants";
import { INDICATOR_POSITIONS } from "../src/indicator.variants";
import SvelteBase from "./fixtures/FrameworkBase.svelte";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
type Scenario = { name: string; options?: Record<string, unknown>; content?: boolean; rich?: boolean };
const scenarios: Scenario[] = [
  ...[undefined, 1, 4 / 3, 21 / 9].map(ratio => ({ name: "AspectRatio", options: { ...(ratio === undefined ? {} : { ratio }), style: { opacity: "0.9" } } })),
  ...SKELETON_SHAPES.flatMap(shape => [undefined, "Loading results"].map(label => ({ name: "Skeleton", options: { shape, label }, content: false }))),
  ...["div", "article", "section"].flatMap(as => ["prose", "content", "full"].flatMap(measure => [false, true].map(striped => ({ name: "Prose", options: { as, measure, striped } })))),
  ...["p", "span", "div"].map(as => ({ name: "Eyebrow", options: { as } })),
  { name: "Kbd", options: { keys: ["Ctrl", "K"], size: "sm" }, content: false },
  { name: "Kbd", options: { keys: ["Alt", "Shift", "P"], separator: " / " }, content: false },
  { name: "Kbd", options: { keys: ["ignored"] } }, { name: "Kbd", content: false },
  { name: "Form" },
  ...[{}, { help: "Choose a team" }, { error: "Selection required" }, { help: "Choose a team", error: "Selection required", disabled: true }].map(messages => ({ name: "Fieldset", options: { legend: "Teams", id: "group", ...messages } })),
  { name: "SkipLink" }, { name: "SkipLink", content: false },
  ...["neutral", "info", "success", "warning", "danger"].flatMap(tone => ["stacked", "inline"].flatMap(layout => [false, true].map(labelHidden => ({ name: "Meter", options: { value: 72, tone, layout, labelHidden }, content: false })))),
  ...["Progress", "RadialProgress"].flatMap(name => [undefined, null, 0, 45, 100, 150, -10, Number.NaN].map(value => ({ name, options: { value, ...(name === "Progress" ? { summary: true } : { size: "lg" }) }, content: false }))),
  ...["Meter", "Progress", "RadialProgress"].flatMap(name => [
    { value: 10, min: 10, max: 10 }, { value: 30, min: 50, max: -10 }, { value: 10, min: Number.NaN, max: Infinity },
    { value: 30, min: 10, max: 50, formatValue: (value: number) => `${value} requests` },
  ].map(options => ({ name, options, content: false }))),
  ...INDICATOR_POSITIONS.map(position => ({ name: "Indicator", options: { position } })),
  ...["neutral", "info", "success", "warning", "danger"].flatMap(tone => [false, true].map(showLabel => ({ name: "StatusIndicator", options: { tone, showLabel, label: "Available" }, content: false }))),
  ...[false, true].flatMap(ordered => ["sm", "md", "lg"].map(gap => ({ name: "List", options: { ordered, gap } }))),
  ...[1, 2, 3, 4].flatMap(columns => [false, true].map(list => ({ name: "GridList", options: { columns, list } }))),
  ...["sm", "md", "lg"].map(gap => ({ name: "DescriptionList", options: { gap, items: [{ term: "Region", detail: "Brazil" }] }, content: false })),
  { name: "DescriptionList", rich: true }, { name: "DescriptionList", content: false },
  ...["horizontal", "vertical"].map(orientation => ({ name: "ControlGroup", options: { label: "Actions", orientation } })),
  ...CONTENT_MASK_SHAPES.map(shape => ({ name: "ContentMask", options: { shape, style: { opacity: "0.9" } } })),
  ...[1, 2, 3, 4, 5, 6].flatMap(level => ["sm", "md", "lg", "display"].map(size => ({ name: "SectionHeading", options: { level, size, title: "Requests", description: "Last hour", titleId: "requests", rule: level % 2 === 0 }, rich: true }))),
  { name: "SectionHeading", options: { title: "Requests" }, content: false },
  { name: "Statistic", options: { label: "Total", value: 1200, description: "This month", formatValue: (value: number | string) => Number(value).toLocaleString("en-US") }, content: false },
  { name: "Statistic", options: { label: "Uptime", value: "99.99%" }, content: false },
];

function reactProps({ name, options = {}, content = true, rich }: Scenario) {
  const props = { ...options, className: "contract-extra", "data-consumer": "forwarded" };
  if (name === "Fieldset") return { ...props, children: (control: FieldsetControlProps) => h("input", { ...control, name: "choice", "aria-label": "Choice", defaultValue: "chosen" }) };
  if (name === "Indicator") return { ...props, marker: h("span", { "aria-label": "3 unread" }, "3"), children: h("button", { type: "button" }, "Inbox") };
  if (name === "SectionHeading") return { ...props, ...(rich ? { actions: h("button", { type: "button" }, "Details") } : {}) };
  if (name === "DescriptionList") return { ...props, ...(rich ? { items: [{ term: h("strong", null, "Requests"), detail: h("a", { href: "/detail" }, "View details") }] } : {}) };
  if (name === "Form") return { ...props, children: [h("input", { key: "input", name: "query", "aria-label": "Query", defaultValue: "original", required: true }), h("button", { key: "submit", type: "submit" }, "Submit"), h("button", { key: "reset", type: "reset" }, "Reset")] };
  if (name === "List" || name === "GridList") return { ...props, children: ["Alpha", "Beta"].map(text => h(options.list === false ? "div" : "li", { key: text }, text)) };
  return { ...props, ...(content ? { children: h("span", { "data-test-content": true }, "Content") } : {}) };
}
async function renderScenario(scenario: Scenario) {
  const svelteTarget = document.createElement("div"); const reactTarget = document.createElement("div"); document.body.append(svelteTarget, reactTarget);
  // Both renderers receive their native style representation.
  const { style, ...attributes } = scenario.options ?? {};
  const options = { ...attributes, ...(style ? { style: Object.entries(style as Record<string, string>).map(([key, value]) => `${key}: ${value}`).join(";") } : {}), class: "contract-extra", "data-consumer": "forwarded" };
  const svelte = mount(SvelteBase, { target: svelteTarget, props: { ...scenario, options } }); flushSync();
  const root = createRoot(reactTarget);
  await act(() => root.render(h((react as unknown as Record<string, ComponentType<Record<string, unknown>>>)[scenario.name]!, reactProps(scenario))));
  return { svelteTarget, reactTarget, update: async (next: Record<string, unknown>) => {
    flushSync(() => svelte.update({ ...next, class: "contract-extra", "data-consumer": "forwarded" }));
    await act(() => root.render(h((react as unknown as Record<string, ComponentType<Record<string, unknown>>>)[scenario.name]!, reactProps({ ...scenario, options: next }))));
  }, cleanup: async () => { await act(() => root.unmount()); await unmount(svelte); svelteTarget.remove(); reactTarget.remove(); } };
}
function snapshot(target: HTMLElement) {
  return [...target.querySelectorAll("*")].map(element => ({ tag: element.tagName, classes: [...element.classList].sort(),
    attributes: Object.fromEntries([...element.attributes].filter(attribute => attribute.name !== "class" && attribute.name !== "style" && attribute.name !== "value" && !attribute.name.startsWith("data-svelte"))
      .map(attribute => [attribute.name, attribute.name.startsWith("data-") && attribute.value === "true" ? "" : attribute.value]).sort()),
    style: element.getAttribute("style") ? Object.fromEntries([...((element as HTMLElement).style)].sort().map(name => [name, (element as HTMLElement).style.getPropertyValue(name)])) : undefined,
    text: element.children.length ? undefined : element.textContent?.trim(),
  }));
}

for (const scenario of scenarios) it(`matches ${scenario.name} public DOM, appearance and semantics ${JSON.stringify(scenario.options ?? {})}`, async () => {
  const { svelteTarget, reactTarget, cleanup } = await renderScenario(scenario);
  try {
    expect(snapshot(reactTarget)).toEqual(snapshot(svelteTarget));
    expect([...reactTarget.querySelectorAll("input")].map(input => input.value)).toEqual([...svelteTarget.querySelectorAll("input")].map(input => input.value));
  } finally { await cleanup(); }
});

for (const framework of ["react", "svelte"] as const) describe(`${framework}: native Base behavior`, () => {
  it("Fieldset updates error relationships, preserves group ids and enforces native disabled behavior", async () => {
    const rendered = await renderScenario({ name: "Fieldset", options: { legend: "Teams", id: "stable-group", help: "Pick a team", error: "Required", disabled: true } });
    try {
      const target = framework === "react" ? rendered.reactTarget : rendered.svelteTarget;
      const input = target.querySelector("input")!;
      expect(input.matches(":disabled")).toBe(true);
      expect(input.getAttribute("aria-invalid")).toBe("true");
      const original = input.getAttribute("aria-describedby")!.split(" ");
      expect(original.map(id => document.getElementById(id)?.textContent)).toEqual(["Pick a team", "Required"]);
      await rendered.update({ legend: "Teams", id: "stable-group", help: "Pick a team" });
      expect(input.matches(":disabled")).toBe(false);
      expect(input.getAttribute("aria-invalid")).toBeNull();
      expect(input.getAttribute("aria-describedby")).toBe(original[0]);
      expect(target.querySelector('[role="alert"]')).toBeNull();
    } finally { await rendered.cleanup(); }
  });

  it("Form preserves native submission, FormData and reset", async () => {
    let submitted = 0;
    const rendered = await renderScenario({ name: "Form", options: framework === "react" ? { onSubmit: (event: Event) => { event.preventDefault(); submitted++; } } : { onsubmit: (event: Event) => { event.preventDefault(); submitted++; } } });
    try {
      const target = framework === "react" ? rendered.reactTarget : rendered.svelteTarget;
      const form = target.querySelector("form")!;
      const input = form.querySelector("input")!;
      input.value = "updated";
      expect(new FormData(form).get("query")).toBe("updated");
      form.querySelector<HTMLButtonElement>('[type="submit"]')!.click();
      expect(submitted).toBe(1);
      form.reset();
      expect(input.value).toBe("original");
    } finally { await rendered.cleanup(); }
  });

  it.each([{}, { cancelled: true }, { malformed: true }, { encoded: true }])("SkipLink respects focus and consumer cancellation %j", async ({ cancelled, malformed, encoded }: { cancelled?: boolean; malformed?: boolean; encoded?: boolean }) => {
    const main = document.createElement("main"); main.id = encoded ? "main content" : "main-content"; main.tabIndex = -1; document.body.append(main);
    const rendered = await renderScenario({ name: "SkipLink", content: false, options: { href: malformed ? "#bad%" : encoded ? "#main%20content" : "#main-content", [framework === "react" ? "onClick" : "onclick"]: cancelled ? (event: Event) => event.preventDefault() : undefined } });
    try {
      const target = framework === "react" ? rendered.reactTarget : rendered.svelteTarget;
      target.querySelector("a")!.click();
      expect(document.activeElement === main).toBe(!cancelled && !malformed);
    } finally { await rendered.cleanup(); main.remove(); }
  });
});
