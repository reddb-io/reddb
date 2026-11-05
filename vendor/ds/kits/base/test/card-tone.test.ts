import { createRawSnippet } from "svelte";
import { describe, expect, it, vi } from "vitest";
import { Card } from "./fixtures/surface-separation-consumer";
import { classes, render, rendered } from "./mount";

// `info` is feedback-info here as on every component (ADR 0026); `brand` is a
// deprecated value kept one release with its accent fill.
const tones = ["neutral", "brand", "success", "warning", "danger", "info"] as const;
const variants = ["outline", "plain"] as const;
const orientations = ["vertical", "horizontal"] as const;

const contracts = {
  neutral: {
    surface: "bg-elevation-raised-surface",
    border: "border-muted",
    foreground: "text-foreground",
    description: "text-ink-muted",
  },
  brand: {
    surface: "bg-[var(--reddb-color-primary)]",
    border: "border-[var(--reddb-color-primary)]",
    foreground: "text-[var(--reddb-color-on-primary)]",
    description: "text-[var(--reddb-color-on-primary)]",
  },
  success: {
    surface: "bg-[var(--reddb-color-feedback-success-surface)]",
    border: "border-[var(--reddb-color-feedback-success-border)]",
    foreground: "text-[var(--reddb-color-feedback-success-foreground)]",
    description: "text-[var(--reddb-color-feedback-success-foreground)]",
  },
  warning: {
    surface: "bg-[var(--reddb-color-feedback-warning-surface)]",
    border: "border-[var(--reddb-color-feedback-warning-border)]",
    foreground: "text-[var(--reddb-color-feedback-warning-foreground)]",
    description: "text-[var(--reddb-color-feedback-warning-foreground)]",
  },
  danger: {
    surface: "bg-[var(--reddb-color-feedback-danger-surface)]",
    border: "border-[var(--reddb-color-feedback-danger-border)]",
    foreground: "text-[var(--reddb-color-feedback-danger-foreground)]",
    description: "text-[var(--reddb-color-feedback-danger-foreground)]",
  },
  info: {
    surface: "bg-[var(--reddb-color-feedback-info-surface)]",
    border: "border-[var(--reddb-color-feedback-info-border)]",
    foreground: "text-[var(--reddb-color-feedback-info-foreground)]",
    description: "text-[var(--reddb-color-feedback-info-foreground)]",
  },
} as const;

const content = createRawSnippet(() => ({ render: () => "<span>Body</span>" }));
const media = createRawSnippet(() => ({ render: () => '<img src="/media.webp" alt="" />' }));

describe("the Base Card tone contract", () => {
  it("warns once in development when the deprecated brand tone is used", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      rendered(render(Card, { tone: "info", title: "Info" }));
      expect(warn).not.toHaveBeenCalled();
      rendered(render(Card, { tone: "brand", title: "Brand" }));
      rendered(render(Card, { tone: "brand", title: "Brand" }));
      expect(warn).toHaveBeenCalledTimes(1);
      expect(String(warn.mock.calls[0]![0])).toMatch(/\[reddb Card\] tone="brand" is deprecated/);
    } finally {
      warn.mockRestore();
    }
  });

  it("composes every token-owned tone with both variants and orientations", () => {
    for (const tone of tones) {
      for (const variant of variants) {
        for (const orientation of orientations) {
          const card = rendered(
            render(Card, {
              tone,
              variant,
              orientation,
              title: "Title",
              description: "Description",
              children: content,
              footer: content,
              media,
            }),
          );
          const contract = contracts[tone];

          expect(card.dataset.tone).toBe(tone);
          expect(classes(card).has(contract.surface), `${tone}/${variant}/${orientation}`).toBe(true);
          expect(classes(card).has(contract.foreground), `${tone}/${variant}/${orientation}`).toBe(true);
          expect(classes(card).has(variant === "outline" ? contract.border : "border-transparent")).toBe(true);

          for (const selector of ["[data-card-header]", "[data-card-title]", "[data-card-body]", "[data-card-footer]"]) {
            expect(classes(card.querySelector(selector)!).has(contract.foreground), `${tone} ${selector}`).toBe(true);
          }
          expect(classes(card.querySelector("[data-card-description]")!).has(contract.description)).toBe(true);

          const mediaClasses = classes(card.querySelector("[data-card-media]")!);
          expect([...mediaClasses].some((name) => name.startsWith("bg-") || name.startsWith("text-"))).toBe(false);
        }
      }
    }
  });
});
