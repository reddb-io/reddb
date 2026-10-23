import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import StatusVocabularyConsumer from "./fixtures/StatusVocabularyConsumer.svelte";
import {
  INDICATOR_POSITIONS,
  Indicator,
  indicator,
} from "./fixtures/status-vocabulary-consumer";
import { classes, classesOf, render, rendered } from "./mount";

const content = createRawSnippet(() => ({ render: () => '<button type="button">Inbox</button>' }));
const marker = createRawSnippet(() => ({ render: () => '<span aria-label="3 unread">3</span>' }));

describe("the Base Indicator", () => {
  it("is available with every placement and its Extension Seam through Base", () => {
    expect(Indicator).toBeDefined();
    expect(indicator).toBeTypeOf("function");
    expect(INDICATOR_POSITIONS).toEqual([
      "top-start",
      "top-center",
      "top-end",
      "middle-start",
      "middle-end",
      "bottom-start",
      "bottom-center",
      "bottom-end",
    ]);
  });

  it("places caller-owned marker content at every canonical anchor", () => {
    const seen = new Set<string>();
    for (const position of INDICATOR_POSITIONS) {
      const root = rendered(render(Indicator, { position, marker, children: content }));
      const placed = root.querySelector<HTMLElement>("[data-indicator-marker]")!;
      expect(classes(placed)).toEqual(classesOf(indicator({ position }).marker()));
      expect(placed.textContent).toBe("3");
      seen.add(placed.className);
    }
    expect(seen.size).toBe(INDICATOR_POSITIONS.length);
  });

  it("preserves the native focus behavior of caller-owned content", () => {
    const root = rendered(render(Indicator, { marker, children: content }));
    const button = root.querySelector<HTMLButtonElement>("button")!;
    expect(root.tabIndex).toBe(-1);
    button.focus();
    expect(document.activeElement).toBe(button);
  });

  it("merges native attributes without selecting any appearance axis", () => {
    const root = rendered(render(Indicator, {
      marker,
      children: content,
      id: "inbox-indicator",
      class: "isolate",
    }));
    const nested = rendered(render(StatusVocabularyConsumer, {}))
      .querySelector<HTMLElement>("[data-indicator]")!;

    expect(root.id).toBe("inbox-indicator");
    expect(classes(root)).toEqual(classesOf(indicator().root({ class: "isolate" })));
    for (const element of [root, nested]) {
      expect(element.hasAttribute("data-theme")).toBe(false);
      expect(element.hasAttribute("data-color-scheme")).toBe(false);
      expect(element.hasAttribute("data-density")).toBe(false);
    }
  });
});
