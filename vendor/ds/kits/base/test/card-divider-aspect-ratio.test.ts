import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import {
  AspectRatio,
  CARD_ACTIONS_ALIGNMENTS,
  CARD_ORIENTATIONS,
  CARD_PADDINGS,
  CARD_VARIANTS,
  Card,
  DEFAULT_ASPECT_RATIO,
  DIVIDER_ORIENTATIONS,
  Divider,
  aspectRatio as aspectRatioAppearance,
  card as cardAppearance,
  divider as dividerAppearance,
} from "./fixtures/surface-separation-consumer";
import CardActionsConsumer from "./fixtures/CardActionsConsumer.svelte";
import CardHorizontalConsumer from "./fixtures/CardHorizontalConsumer.svelte";
import SurfaceSeparationConsumer from "./fixtures/SurfaceSeparationConsumer.svelte";
import TestGlyph from "./fixtures/TestGlyph.svelte";
import { classes, classesOf, render, rendered, styleOf } from "./mount";

const text = (value: string) =>
  createRawSnippet(() => ({ render: () => `<span>${value}</span>` }));

describe("the Base Card", () => {
  it("offers complete edge and Density-owned padding vocabularies", () => {
    expect(CARD_VARIANTS).toEqual(["outline", "plain"]);
    expect(CARD_PADDINGS).toEqual(["none", "sm", "md"]);
    expect(CARD_ACTIONS_ALIGNMENTS).toEqual(["start", "center", "end", "between"]);
    expect(CARD_ORIENTATIONS).toEqual(["vertical", "horizontal"]);

    for (const variant of CARD_VARIANTS) {
      for (const padding of CARD_PADDINGS) {
        const element = rendered(render(Card, { variant, padding }));
        expect(classes(element)).toEqual(
          classesOf(cardAppearance({ variant, padding }).root()),
        );
      }
    }
  });

  it("keeps every section inset live against the nearest Density scope", () => {
    for (const padding of ["sm", "md"] as const) {
      const element = rendered(
        render(Card, {
          padding,
          title: "Surface",
          children: text("Body"),
          footer: text("Footer"),
        }),
      );
      const tokenClass = `p-[var(--reddb-spatial-inset-${padding})]`;

      for (const section of element.children) expect(classes(section).has(tokenClass)).toBe(true);
      expect(element.hasAttribute("data-density")).toBe(false);
    }
  });

  it("omits absent sections and keeps caller content in document order", () => {
    const bodyOnly = rendered(render(Card, { children: text("Body") }));
    expect(bodyOnly.children).toHaveLength(1);

    const complete = rendered(
      render(Card, { title: "Header", children: text("Body"), footer: text("Footer") }),
    );
    expect(classes(complete.querySelector("[data-card-footer]")!).has("justify-start")).toBe(true);
    expect([...complete.children].map((section) => section.textContent?.trim())).toEqual([
      "Header",
      "Body",
      "Footer",
    ]);
  });

  it("aligns multiple footer actions without consumer layout classes", () => {
    const alignments = {
      start: "justify-start",
      center: "justify-center",
      end: "justify-end",
      between: "justify-between",
    } as const;

    for (const [actionsAlign, expectedClass] of Object.entries(alignments)) {
      const element = rendered(render(CardActionsConsumer, { actionsAlign }));
      const footer = element.querySelector<HTMLElement>("[data-card-footer]")!;

      expect(classes(footer).has(expectedClass)).toBe(true);
      expect([...footer.querySelectorAll("button")].map((button) => button.className)).toEqual([
        "",
        "",
      ]);
    }
  });

  it("places media beside an ordered content column with a narrow-container fallback", () => {
    const card = rendered(render(CardHorizontalConsumer));
    const layout = card.querySelector<HTMLElement>("[data-card-layout]")!;
    const media = card.querySelector<HTMLElement>("[data-card-media]")!;
    const content = card.querySelector<HTMLElement>("[data-card-content]")!;

    expect([...layout.children]).toEqual([media, content]);
    expect(
      [...content.children].map((section) =>
        ["header", "body", "footer"].find((name) =>
          section.hasAttribute(`data-card-${name}`),
        ),
      ),
    ).toEqual(["header", "body", "footer"]);
    expect([...content.children].map((section) => section.textContent?.trim())).toEqual([
      "Release Ready to ship",
      "Review the changes.",
      "Publish",
    ]);
    for (const expected of ["@container/card", "overflow-hidden"]) {
      expect(classes(card).has(expected)).toBe(true);
    }
    for (const expected of ["flex", "flex-col", "@sm/card:flex-row"]) {
      expect(classes(layout).has(expected)).toBe(true);
    }
    expect(classes(media).has("@sm/card:w-2/5")).toBe(true);
  });

  it("renders a decorative title icon through Icon unless a full header replaces it", () => {
    const standard = rendered(render(Card, { icon: TestGlyph, title: "Release" }));
    const icon = standard.querySelector<SVGElement>("[data-card-header] [data-icon]")!;

    expect(icon.hasAttribute("data-test-glyph")).toBe(true);
    expect(icon.getAttribute("aria-hidden")).toBe("true");

    const overridden = rendered(
      render(Card, { icon: TestGlyph, title: "Ignored", header: text("Custom header") }),
    );
    expect(overridden.querySelector("[data-icon]")).toBeNull();
    expect(overridden.querySelector("[data-card-header]")?.textContent?.trim()).toBe(
      "Custom header",
    );
  });
});

describe("the Base Divider", () => {
  it("exposes its visual direction as separator semantics", () => {
    expect(DIVIDER_ORIENTATIONS).toEqual(["horizontal", "vertical"]);
    for (const orientation of DIVIDER_ORIENTATIONS) {
      const element = rendered(render(Divider, { orientation }));
      expect(element.getAttribute("role")).toBe("separator");
      expect(element.getAttribute("aria-orientation")).toBe(orientation);
      expect(classes(element)).toEqual(classesOf(dividerAppearance({ orientation })));
    }
  });

  it("stays outside the keyboard order", () => {
    const element = rendered(render(Divider, {}));
    expect(element.tabIndex).toBe(-1);
  });
});

describe("the Base AspectRatio", () => {
  it("defaults to 16:9 while accepting an arbitrary caller ratio", () => {
    const standard = rendered(render(AspectRatio, {}));
    const portrait = rendered(render(AspectRatio, { ratio: 3 / 4, class: "max-w-md" }));

    expect(styleOf(standard, "aspect-ratio")).toBe(`${DEFAULT_ASPECT_RATIO} / 1`);
    expect(styleOf(portrait, "aspect-ratio")).toBe(`${3 / 4} / 1`);
    expect(classes(portrait)).toEqual(
      classesOf(aspectRatioAppearance({ class: "max-w-md" })),
    );
  });

  it("preserves focusable caller content", () => {
    const root = render(AspectRatio, {
      ratio: 1,
      children: createRawSnippet(() => ({
        render: () => '<button type="button">Open media</button>',
      })),
    });
    const button = root.querySelector<HTMLButtonElement>("button")!;

    button.focus();
    expect(document.activeElement).toBe(button);
  });
});

describe("surface and separation composition", () => {
  it("inherits a nested appearance without selecting any axis", () => {
    const root = render(SurfaceSeparationConsumer);
    const scope = root.querySelector<HTMLElement>("[data-surface-appearance-scope]")!;
    const capabilities = scope.querySelectorAll<HTMLElement>(
      "[data-card], [data-divider], [data-aspect-ratio]",
    );

    expect(scope.getAttribute("data-theme")).toBe("marketing");
    expect(scope.getAttribute("data-color-scheme")).toBe("dark");
    expect(scope.getAttribute("data-density")).toBe("compact");
    expect(capabilities.length).toBeGreaterThanOrEqual(4);
    for (const capability of capabilities) {
      expect(capability.hasAttribute("data-theme")).toBe(false);
      expect(capability.hasAttribute("data-color-scheme")).toBe(false);
      expect(capability.hasAttribute("data-density")).toBe(false);
    }
  });
});
