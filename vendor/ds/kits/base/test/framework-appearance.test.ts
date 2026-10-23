import { mount, unmount, flushSync } from "svelte";
import { act, createElement as h, type ComponentType } from "react";
import { createRoot } from "react-dom/client";
import { expect, it } from "vitest";
import * as react from "../react/src";
import SveltePrimitive from "./fixtures/FrameworkPrimitive.svelte";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
const scenarios: Array<{ name: string; options?: Record<string, unknown> }> = [
  ...["neutral", "info", "success", "warning", "danger"].flatMap(tone => ["tinted", "filled", "outline"].map(variant => ({ name: "Badge", options: { tone, variant } }))),
  ...["primary", "secondary", "ghost"].flatMap(variant => ["sm", "md", "lg"].map(size => ({ name: "Button", options: { variant, size } }))),
  { name: "Button", options: { href: "/detail", disabled: true } },
  { name: "Button", options: { href: "/detail" } },
  { name: "Button", options: { loading: true } },
  { name: "Button", options: { block: true, tone: "danger" } },
  ...["prose", "content", "wide", "full"].map(size => ({ name: "Container", options: { size } })),
  ...["horizontal", "vertical"].map(orientation => ({ name: "Divider", options: { orientation } })),
  ...[1, 2, 3, 4, 5, 6].map(level => ({ name: "Heading", options: { level } })),
  { name: "Label", options: { for: "named-control" } },
  ...["sm", "md", "lg"].flatMap(size => [false, true].map(labelHidden => ({ name: "LoadingIndicator", options: { size, labelHidden, label: "Fetching results" } }))),
  ...["row", "column"].flatMap(direction => [false, true].map(wrap => ({ name: "Stack", options: { direction, wrap, gap: "layout-sm", align: "center", justify: "between" } }))),
  ...["body", "caption"].flatMap(role => ["p", "span", "div", "small"].map(as => ({ name: "Text", options: { role, as } }))),
];

function snapshot(target: HTMLElement) {
  // Compare the public DOM contract and recipes, ignoring renderer markers and
  // Svelte's comment anchors. Every nested spinner/label still participates.
  return [...target.querySelectorAll("*")].map(element => ({
    tag: element.tagName,
    classes: [...element.classList].sort(),
    attributes: Object.fromEntries([...element.attributes]
      .filter(attribute => /^(aria-|role$|type$|href$|tabindex$|disabled$|for$|data-(badge|tone|variant|container-size|divider|heading|type-role|loading-indicator|stack|text)$)/.test(attribute.name))
      .map(attribute => [attribute.name, attribute.name.startsWith("data-") && attribute.value === "true" ? "" : attribute.value]).sort()),
    text: element.children.length ? undefined : element.textContent?.trim(),
  }));
}

for (const { name, options = {} } of scenarios) it(`shares ${name} appearance and native semantics ${JSON.stringify(options)}`, async () => {
  const svelteTarget = document.createElement("div"); const reactTarget = document.createElement("div");
  document.body.append(svelteTarget, reactTarget);
  const svelte = mount(SveltePrimitive, { target: svelteTarget, props: { name, options } }); flushSync();
  const root = createRoot(reactTarget);
  try {
    const { for: htmlFor, ...native } = options;
    await act(() => root.render(h((react as unknown as Record<string, ComponentType<Record<string, unknown>>>)[name]!, { ...native, ...(htmlFor ? { htmlFor } : {}), ...(name === "Divider" ? {} : { children: "Content" }) })));
    expect(snapshot(reactTarget)).toEqual(snapshot(svelteTarget));
    if (name === "LoadingIndicator") {
      expect(reactTarget.querySelector('[role="status"]')?.textContent).toContain("Fetching results");
      expect(reactTarget.querySelector('[aria-live="polite"]')).not.toBeNull();
    }
    if (name === "Button" && options.disabled) expect(reactTarget.querySelector("a")?.getAttribute("aria-disabled")).toBe("true");
  } finally { await act(() => root.unmount()); await unmount(svelte); svelteTarget.remove(); reactTarget.remove(); }
});
