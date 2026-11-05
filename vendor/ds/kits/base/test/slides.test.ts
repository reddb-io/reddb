// Presentations (ADR 0027): SlideFrame, the slide layouts and Deck.
//
// What is pinned here is the markup contract jsdom can see: the frame is a
// named "slide" group carrying `data-slide-frame` (the attribute the Theme's
// print rules key on), it scales a fixed canvas with `zoom` rather than a
// transform, it pins the layout and section tiers to the neutral stop's Brand
// steps without selecting an axis, and every layout takes its type from the
// Theme's roles. Geometry — the 16:9 ratio, scaling, overflow — is measured in
// a real browser by the showcase (`slide-frame.browser.test.ts`).

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { createRawSnippet, type Component } from "svelte";
import { describe, expect, it } from "vitest";
import {
  BulletsSlide,
  Deck,
  FigureSlide,
  QuoteSlide,
  SLIDE_SURFACES,
  SectionSlide,
  SlideFrame,
  TitleSlide,
  TwoColumnSlide,
  slideFrame,
} from "@reddb-io/design-system/base";
import { classes, classesOf, render, rendered } from "./mount";

const words = (content: string) => createRawSnippet(() => ({ render: () => `<span>${content}</span>` }));
const DENSITY_DIR = join(import.meta.dirname, "..", "..", "..", "packages", "tokens", "dist");

function expectNoAxis(target: HTMLElement) {
  for (const attribute of ["data-theme", "data-color-scheme", "data-density"]) {
    expect(target.querySelector(`[${attribute}]`), attribute).toBeNull();
  }
}

/** Every layout, with the props it needs to render. */
const LAYOUTS: readonly { name: string; component: Component<never>; props: Record<string, unknown>; titleRole?: string }[] = [
  {
    name: "TitleSlide",
    component: TitleSlide as Component<never>,
    props: { title: "One system", eyebrow: "reddb.io", subtitle: "Every medium", presenter: "The DS team" },
    titleRole: "display",
  },
  { name: "SectionSlide", component: SectionSlide as Component<never>, props: { title: "Part two", summary: "Media" }, titleRole: "display" },
  {
    name: "TwoColumnSlide",
    component: TwoColumnSlide as Component<never>,
    props: { title: "Before and after", start: words("before"), end: words("after") },
    titleRole: "title",
  },
  {
    name: "FigureSlide",
    component: FigureSlide as Component<never>,
    props: { title: "Readiness by medium", figure: words("chart"), caption: "Audit, 2026-10-05" },
    titleRole: "title",
  },
  { name: "QuoteSlide", component: QuoteSlide as Component<never>, props: { quote: "Tokens only.", attribution: "AGENTS.md" } },
  {
    name: "BulletsSlide",
    component: BulletsSlide as Component<never>,
    props: { title: "Non-negotiables", items: ["Tokens only", "Density owns spatial values"] },
    titleRole: "title",
  },
];

describe("the Base SlideFrame", () => {
  it("is a named slide group carrying the print hook", () => {
    const frame = rendered(render(SlideFrame, { label: "Opening", children: words("Hello") }));
    expect(frame.getAttribute("role")).toBe("group");
    expect(frame.getAttribute("aria-roledescription")).toBe("slide");
    expect(frame.getAttribute("aria-label")).toBe("Opening");
    expect(frame.hasAttribute("data-slide-frame")).toBe(true);
    expect(frame.querySelector("[data-slide-safe-area]")?.textContent).toBe("Hello");
  });

  it("is a 16:9 size container that zooms a fixed canvas instead of transforming it", () => {
    const frame = rendered(render(SlideFrame, { label: "Canvas" }));
    const stage = frame.querySelector("[data-slide-stage]")!;
    for (const name of ["@container/slide-frame", "aspect-video", "w-full", "overflow-hidden", "break-inside-avoid"]) {
      expect(classes(frame).has(name), name).toBe(true);
    }
    for (const name of ["w-[60rem]", "aspect-video", "[zoom:tan(atan2(100cqi,60rem))]", "@container/slide"]) {
      expect(classes(stage).has(name), name).toBe(true);
    }
    const all = [...classes(frame), ...classes(stage)];
    expect(all.filter((name) => /^(?:scale-|transform|\[transform)/.test(name))).toEqual([]);
  });

  it("pins the layout and section tiers to the comfortable stop's Brand steps", () => {
    const comfortable = readFileSync(join(DENSITY_DIR, "density-comfortable.css"), "utf8");
    const stage = rendered(render(SlideFrame, { label: "Rhythm" })).querySelector("[data-slide-stage]")!;
    const pinned = [...classes(stage)]
      .map((name) => /^\[(--reddb-spatial-[a-z-]+):(.+)\]$/.exec(name))
      .filter((match): match is RegExpExecArray => match !== null);

    expect(pinned.map((match) => match[1]).sort()).toEqual([
      "--reddb-spatial-layout-gap-lg",
      "--reddb-spatial-layout-gap-md",
      "--reddb-spatial-layout-gap-sm",
      "--reddb-spatial-section-gap",
      "--reddb-spatial-section-inset",
    ]);
    for (const [, property, value] of pinned) {
      expect(comfortable, property).toContain(`${property}: ${value};`);
    }
  });

  it("shows its position in the deck and announces it in words", () => {
    const frame = rendered(render(SlideFrame, { label: "Third", number: 3, total: 8 }));
    const number = frame.querySelector("[data-slide-number-label]")!;
    expect(frame.dataset.slideNumber).toBe("3");
    expect(frame.dataset.slideTotal).toBe("8");
    expect(number.querySelector(".sr-only")?.textContent).toBe("Slide 3 of 8");
    expect(number.querySelector('[aria-hidden="true"]')?.textContent).toBe("3 / 8");
    expect(rendered(render(SlideFrame, { label: "Unnumbered" })).querySelector("[data-slide-number-label]")).toBeNull();
  });

  it("sets a slide on the background or the raised ground without selecting an axis", () => {
    expect([...SLIDE_SURFACES]).toEqual(["background", "raised"]);
    for (const surface of SLIDE_SURFACES) {
      const target = render(SlideFrame, { label: surface, surface });
      const frame = rendered(target);
      expect(frame.dataset.slideSurface).toBe(surface);
      expect(classes(frame)).toEqual(classesOf(slideFrame({ surface }).root()));
      expectNoAxis(target);
    }
    expect(classes(rendered(render(SlideFrame, { label: "Default" }))).has("bg-background")).toBe(true);
  });
});

describe("the Base slide layouts", () => {
  for (const { name, component, props, titleRole } of LAYOUTS) {
    it(`${name} is a SlideFrame whose type comes only from the Theme's roles`, () => {
      const target = render(component, { ...props, number: 2, total: 6 } as never);
      const frame = rendered(target);
      expect(frame.hasAttribute("data-slide-frame")).toBe(true);
      expect(frame.getAttribute("aria-roledescription")).toBe("slide");
      expect(frame.getAttribute("aria-label")).toBeTruthy();
      expect(frame.dataset.slideNumber).toBe("2");
      expectNoAxis(target);

      for (const element of [frame, ...frame.querySelectorAll("*")]) {
        const raw = [...classes(element)].filter((cls) =>
          /^(?:text-(?:xs|sm|base|lg|\d*xl|display-(?:sm|md|lg)|\[.*\])|font-(?:semibold|light|black|\[.*\])|tracking-.+|leading-.+)$/.test(cls),
        );
        expect(raw, `${name}: ${element.tagName}`).toEqual([]);
      }

      if (titleRole) {
        const heading = frame.querySelector("[data-heading]") as HTMLElement;
        expect(heading.tagName).toBe("H2");
        expect(heading.dataset.typeRole).toBe(titleRole);
        expect(frame.getAttribute("aria-label")).toBe(props.title);
      }
    });
  }

  it("lets a caller name the slide and choose the title's outline level", () => {
    const frame = rendered(render(TitleSlide, { title: "Deck", level: 1, label: "Opening slide" }));
    expect(frame.getAttribute("aria-label")).toBe("Opening slide");
    expect(frame.querySelector("h1")?.textContent).toBe("Deck");
  });

  it("places TwoColumnSlide's columns in reading order", () => {
    const frame = rendered(render(TwoColumnSlide, { title: "Columns", start: words("first"), end: words("second") }));
    expect([...frame.querySelectorAll<HTMLElement>("[data-slide-column]")].map((column) => [column.dataset.slideColumn, column.textContent])).toEqual([
      ["start", "first"],
      ["end", "second"],
    ]);
  });

  it("gives FigureSlide's figure the remaining height and a native caption", () => {
    const frame = rendered(render(FigureSlide, { title: "Figure", figure: words("chart"), caption: "Source: audit" }));
    const figure = frame.querySelector("figure[data-slide-figure]")!;
    expect(figure.querySelector("[data-slide-figure-body]")?.textContent).toBe("chart");
    expect(classes(figure.querySelector("[data-slide-figure-body]")!).has("flex-1")).toBe(true);
    expect(figure.querySelector("figcaption")?.textContent).toBe("Source: audit");
  });

  it("sets QuoteSlide as a blockquote with its attribution, in the title role", () => {
    const frame = rendered(render(QuoteSlide, { quote: "Tokens only.", attribution: "AGENTS.md", source: "Non-negotiables" }));
    expect(frame.getAttribute("aria-label")).toBe("Quote from AGENTS.md");
    const quote = frame.querySelector("blockquote p")!;
    expect(quote.textContent).toBe("“Tokens only.”");
    expect(classes(quote).has("text-title")).toBe(true);
    expect(frame.querySelector("figcaption")?.textContent?.replace(/\s+/g, " ").trim()).toBe("AGENTS.md, Non-negotiables");
  });

  it("lists BulletsSlide's points natively, in the heading role", () => {
    const frame = rendered(render(BulletsSlide, { title: "Points", items: ["One", "Two", "Three"] }));
    const list = frame.querySelector("ul[data-slide-points]")!;
    expect([...list.querySelectorAll("li")].map((item) => item.textContent)).toEqual(["One", "Two", "Three"]);
    expect(classes(list).has("text-heading")).toBe(true);
  });
});

describe("the Base Deck", () => {
  it("is a named region stacking its slides with a Density-owned layout gap", () => {
    const deck = rendered(render(Deck, { label: "About the DS", children: words("slides") }));
    expect(deck.tagName).toBe("SECTION");
    expect(deck.getAttribute("aria-label")).toBe("About the DS");
    expect(deck.hasAttribute("data-deck")).toBe(true);
    expect(classes(deck).has("gap-[var(--reddb-spatial-layout-gap-lg)]")).toBe(true);
  });
});
