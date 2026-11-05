// Mounting a Kit component the way an application does.
//
// Svelte 5's `mount` is the whole harness: it takes a component and a target
// and gives back the instance, which is enough to render and assert against
// real DOM. No testing-library sits in between, deliberately — the thing under
// test renders one element wearing known classes around one <img>, and that
// contract is read off the DOM directly. The application Kit's `test/mount.ts`
// is the same harness for the same reason; it stays a per-Kit file because a
// Kit's tests are the one thing about a Kit that never leaves it.
//
// `withColorScheme` writes the contrast attribute the Logo follows when `on`
// is omitted. The write lands on the same document the component mounts into,
// exactly as an application's Color Scheme switch does. Theme remains free to
// describe direction independently, and this helper never mutates that axis.
// The restoration therefore keeps the original harness coverage intact.

import { afterEach, expect } from "vitest";
import { flushSync, mount, unmount, type Component } from "svelte";

const mounted: { instance: Record<string, unknown>; target: HTMLElement }[] = [];

/** Mount `component` with `props` into a fresh detached element. */
export function render<Props extends Record<string, unknown>>(
  component: Component<Props>,
  props: Props = {} as Props,
): HTMLElement {
  const target = document.createElement("div");
  document.body.appendChild(target);
  const instance = mount(component, { target, props }) as Record<string, unknown>;
  mounted.push({ instance, target });
  flushSync();
  return target;
}

/** The single root element the component rendered. */
export function rendered(target: HTMLElement): HTMLElement {
  const element = target.firstElementChild;
  expect(element, "the component rendered no element").not.toBeNull();
  return element as HTMLElement;
}

/** A class string as a set, so assertions never depend on class order. */
export function classesOf(className: string): Set<string> {
  return new Set(className.split(/\s+/).filter(Boolean));
}

/** The element's classes as a set, read off the attribute. */
export function classes(element: Element): Set<string> {
  return classesOf(element.getAttribute("class") ?? "");
}

/** One declaration out of an inline style attribute, e.g. `padding`. */
export function styleOf(element: Element, property: string): string {
  return (element as HTMLElement).style.getPropertyValue(property);
}

/** The active Color Scheme on the test document — what `on` defaults from. */
export const COLOR_SCHEME_ATTRIBUTE = "data-color-scheme";

/** Run `body` with `scheme` active on the document, then put it back. */
export function withColorScheme<T>(scheme: string | null, body: () => T): T {
  const root = document.documentElement;
  const before = root.getAttribute(COLOR_SCHEME_ATTRIBUTE);
  if (scheme === null) root.removeAttribute(COLOR_SCHEME_ATTRIBUTE);
  else root.setAttribute(COLOR_SCHEME_ATTRIBUTE, scheme);
  try {
    return body();
  } finally {
    if (before === null) root.removeAttribute(COLOR_SCHEME_ATTRIBUTE);
    else root.setAttribute(COLOR_SCHEME_ATTRIBUTE, before);
  }
}

afterEach(() => {
  for (const { instance, target } of mounted.splice(0)) {
    unmount(instance);
    target.remove();
  }
});
