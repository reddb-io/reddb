// Mounting a Kit component the way an application does.
//
// Svelte 5's `mount` is the whole harness: it takes a component and a target
// and gives back the instance, which is enough to render, interact and assert
// against real DOM. No testing-library sits in between, deliberately — the
// thing under test is a Primitive whose entire contract is "renders this
// element, wearing these classes, passing everything else through", and that
// contract is read off the DOM directly.

import { afterEach, expect } from "vitest";
import {
  createRawSnippet,
  flushSync,
  mount,
  unmount,
  type Component,
  type Snippet,
} from "svelte";

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

/**
 * Click `element` and let Svelte settle before the assertions run.
 *
 * `element.click()` rather than a dispatched MouseEvent, because the platform's
 * click steps are part of what is under test: a disabled control swallows a
 * click there, and a synthetic event dispatched straight at the element would
 * sail past that and prove the opposite of what it looks like it proves.
 */
export function click(element: HTMLElement): void {
  element.click();
  flushSync();
}

/** A class string as a set, so assertions never depend on class order. */
export function classesOf(className: string): Set<string> {
  return new Set(className.split(/\s+/).filter(Boolean));
}

/**
 * The element's classes as a set.
 *
 * Read off the attribute rather than off `className`, because on an SVG
 * element — such as an inline SVG — `className` is an
 * `SVGAnimatedString` and not a string at all.
 */
export function classes(element: Element): Set<string> {
  return classesOf(element.getAttribute("class") ?? "");
}

/** Content for a component's `children`, as a consumer's markup would be. */
export function text(content: string): Snippet {
  return createRawSnippet(() => ({ render: () => `<span>${content}</span>` }));
}

afterEach(async () => {
  for (const { instance, target } of mounted.splice(0)) {
    unmount(instance);
    target.remove();
  }

  // Bits UI restores its shared body scroll lock 24ms after the final
  // overlay is destroyed. Keep jsdom alive through that bounded cleanup so
  // the callback cannot outlive the document when test files run in parallel.
  await new Promise<void>((resolve) => window.setTimeout(resolve, 30));
});
