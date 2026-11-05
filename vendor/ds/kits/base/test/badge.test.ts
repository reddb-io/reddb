import { createRawSnippet } from "svelte";
import { describe, expect, it, vi } from "vitest";
import StatusVocabularyConsumer from "./fixtures/StatusVocabularyConsumer.svelte";
import {
  BADGE_VARIANTS,
  Badge,
  DEPRECATED_BADGE_VARIANTS,
  TONES,
  badge,
} from "./fixtures/status-vocabulary-consumer";
import { classes, classesOf, render, rendered } from "./mount";

const label = (value: string) => createRawSnippet(() => ({ render: () => `<span>${value}</span>` }));

describe("the Base Badge", () => {
  it("is available with every emphasis and its Extension Seam through Base", () => {
    expect(Badge).toBeDefined();
    expect(badge).toBeTypeOf("function");
    // Emphasis (`variant`) and meaning (`tone`) are two axes (ADR 0026).
    expect(BADGE_VARIANTS).toEqual(["tinted", "filled", "outline"]);
    expect(Object.keys(DEPRECATED_BADGE_VARIANTS)).toEqual(["neutral", "primary"]);
  });

  it("draws every tone at every emphasis from its Feedback Role, never the accent", () => {
    for (const variant of BADGE_VARIANTS) {
      for (const tone of TONES) {
        const element = rendered(render(Badge, { tone, variant, children: label(tone) }));
        const worn = [...classes(element)];
        expect(classes(element), `${tone}/${variant}`).toEqual(classesOf(badge({ tone, variant })));
        expect(element.dataset.tone).toBe(tone);
        expect(worn.some((name) => /(?:^|-)primary$/.test(name)), `${tone}/${variant}`).toBe(false);
        if (tone !== "neutral") {
          expect(worn.some((name) => name.includes(`feedback-${tone}-`)), `${tone}/${variant}`).toBe(true);
        }
      }
    }
    // Tinted is the role's copy on its tinted surface; filled is a tone chip.
    expect(classesOf(badge({ tone: "success" }))).toEqual(
      classesOf(badge({ tone: "success", variant: "tinted" })),
    );
    expect([...classesOf(badge({ tone: "danger", variant: "filled" }))]).toEqual(
      expect.arrayContaining(["bg-feedback-danger-foreground", "text-on-feedback"]),
    );
  });

  it("keeps the deprecated neutral and primary variants working for one release, with a warning", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const neutral = rendered(render(Badge, { variant: "neutral", children: label("Draft") }));
      expect(classes(neutral)).toEqual(classesOf(badge({ variant: "tinted" })));
      const primary = rendered(render(Badge, { variant: "primary", children: label("New") }));
      expect([...classes(primary)]).toEqual(expect.arrayContaining(["bg-primary", "text-on-primary"]));
      rendered(render(Badge, { variant: "primary", children: label("Again") }));
      expect(warn).toHaveBeenCalledTimes(2);
      expect(String(warn.mock.calls[0]![0])).toMatch(/\[reddb Badge\] variant="neutral" is deprecated/);
      expect(String(warn.mock.calls[1]![0])).toMatch(/\[reddb Badge\] variant="primary" is deprecated.*variant="filled"/);
    } finally {
      warn.mockRestore();
    }
  });

  it("renders required caller-owned status text inside running text", () => {
    for (const variant of BADGE_VARIANTS) {
      const element = rendered(render(Badge, { variant, children: label(`${variant} release`) }));
      expect(element.tagName).toBe("SPAN");
      expect(element.textContent).toBe(`${variant} release`);
      expect(classes(element)).toEqual(classesOf(badge({ variant })));
    }
  });

  it("does not become a control while preserving native attributes", () => {
    const element = rendered(render(Badge, {
      children: label("Beta"),
      id: "release-stage",
      title: "Release stage",
    }));
    expect(element.tabIndex).toBe(-1);
    expect(element.id).toBe("release-stage");
    expect(element.title).toBe("Release stage");
  });

  it("keeps its Density role live and inherits every nested appearance axis", () => {
    const element = rendered(render(Badge, {
      children: label("Stable"),
      class: "uppercase",
    }));
    const nested = rendered(render(StatusVocabularyConsumer, {})).querySelector<HTMLElement>(
      "[data-badge]",
    )!;

    expect(classes(element)).toEqual(classesOf(badge({ class: "uppercase" })));
    expect(classes(element).has("gap-[var(--reddb-spatial-gap-sm)]")).toBe(true);
    for (const root of [element, nested]) {
      expect(root.hasAttribute("data-theme")).toBe(false);
      expect(root.hasAttribute("data-color-scheme")).toBe(false);
      expect(root.hasAttribute("data-density")).toBe(false);
    }
  });
});
