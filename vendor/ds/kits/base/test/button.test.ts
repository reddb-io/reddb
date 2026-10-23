// Button at the Base subpath, through the same public seam a consumer uses.
//
// These assertions preserve the observable contract moved out of Application.
// Base is now its only implementation and public export.

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { createRawSnippet, flushSync, type Snippet } from "svelte";
import { describe, expect, it, vi } from "vitest";
import {
  APPEARANCE_THEMES,
  BASE_THEME,
  COLOR_SCHEMES,
} from "../../../packages/theme/src/appearance";
import { appearanceThemeCssPath, colorSchemeCssPath } from "../../../packages/theme/src/paths";
import {
  cascadeFor,
  parseStylesheets,
  resolveProperty,
} from "../../../packages/theme/test/support/cascade";
import {
  BUTTON_INTENTS,
  BUTTON_SIZES,
  BUTTON_TONES,
  BUTTON_VARIANTS,
  TONES,
  Button,
  button,
  buttonSpinner,
} from "./fixtures/button-consumer";
import { classes, classesOf, render, rendered } from "./mount";

function text(content: string): Snippet {
  return createRawSnippet(() => ({ render: () => `<span>${content}</span>` }));
}

function click(element: HTMLElement): void {
  element.click();
  flushSync();
}

const composition = parseStylesheets([
  readFileSync(join(import.meta.dirname, "..", "..", "..", "packages", "tokens", "dist", "tokens.css"), "utf8"),
  readFileSync(appearanceThemeCssPath(BASE_THEME), "utf8"),
  ...APPEARANCE_THEMES.map((theme) => readFileSync(appearanceThemeCssPath(theme), "utf8")),
  ...COLOR_SCHEMES.map((scheme) => readFileSync(colorSchemeCssPath(scheme), "utf8")),
]);

function channel(hex: string, offset: number): number {
  const value = Number.parseInt(hex.slice(offset, offset + 2), 16) / 255;
  return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
}

function luminance(hex: string): number {
  return 0.2126 * channel(hex, 1) + 0.7152 * channel(hex, 3) + 0.0722 * channel(hex, 5);
}

function contrast(left: string, right: string): number {
  const [lighter, darker] = [luminance(left), luminance(right)].sort((a, b) => b - a);
  return (lighter + 0.05) / (darker + 0.05);
}

describe("the Base Button", () => {
  it("is available through the Base consumer subpath", () => {
    expect(Button).toBeDefined();
    expect(button).toBeTypeOf("function");
    expect(buttonSpinner).toBeTypeOf("function");
    // Button's meaning is the shared tone vocabulary (ADR 0026); the old
    // `BUTTON_INTENTS` name stays one release as the same list.
    expect(BUTTON_TONES).toEqual(TONES);
    expect(BUTTON_INTENTS).toBe(BUTTON_TONES);
  });

  it("renders a <button> that does not submit its form by accident", () => {
    const element = rendered(render(Button, {}));
    expect(element.tagName).toBe("BUTTON");
    expect(element.getAttribute("type")).toBe("button");
  });

  it("renders its children", () => {
    const element = rendered(render(Button, { children: text("Save") }));
    expect(element.textContent?.trim()).toBe("Save");
  });

  it("wears exactly the classes its exported variants produce", () => {
    for (const variant of BUTTON_VARIANTS) {
      for (const size of BUTTON_SIZES) {
        const element = rendered(render(Button, { variant, size }));
        expect(classes(element)).toEqual(classesOf(button({ variant, size })));
      }
    }
  });

  it("defaults to the secondary variant at the medium size", () => {
    const element = rendered(render(Button, {}));
    expect(classes(element)).toEqual(classesOf(button({ variant: "secondary", size: "md" })));
  });

  it("composes every tone with every emphasis using only its token family", () => {
    // A feedback intent keeps the emphasis hierarchy: primary fills with the
    // role's fill material under its on-fill label, secondary outlines in its
    // border, ghost carries only its ink — so the three emphases stay
    // distinguishable (ADR 0009). Every one focuses with the one ink ring,
    // offset from the control (ADR 0023).
    const FOCUS = ["focus-visible:outline-2", "focus-visible:outline-offset-2", "focus-visible:outline-focus"];
    const feedback = (role: string) => ({
      primary: [`bg-feedback-${role}-fill`, `text-feedback-${role}-on-fill`, ...FOCUS],
      secondary: [
        `border-feedback-${role}-border`,
        `text-feedback-${role}-foreground`,
        `hover:bg-feedback-${role}-surface`,
        ...FOCUS,
      ],
      ghost: [`text-feedback-${role}-foreground`, `hover:bg-feedback-${role}-surface`, ...FOCUS],
    });
    const contracts = {
      neutral: {
        // Hover, press and an open popup fill with the opaque primary-active
        // role the contrast contract pairs with on-primary (wave 5A).
        primary: ["bg-primary", "text-on-primary", "hover:bg-primary-active", "aria-expanded:bg-primary-active", ...FOCUS],
        secondary: ["border-control-edge", "bg-transparent", "text-foreground", ...FOCUS],
        ghost: ["bg-transparent", "text-ink-muted", ...FOCUS],
      },
      danger: feedback("danger"),
      success: feedback("success"),
      warning: feedback("warning"),
      info: feedback("info"),
    } as const;

    for (const [intent, variants] of Object.entries(contracts)) {
      for (const [variant, expected] of Object.entries(variants)) {
        const actual = classesOf(button({ tone: intent, variant } as never));
        for (const token of expected) expect(actual.has(token), `${intent}/${variant}: ${token}`).toBe(true);
        expect([...actual], `${intent}/${variant}`).not.toContainEqual(expect.stringMatching(/(?:#[\da-f]{3,8}|(?:rgb|hsl)a?\()/i));
        // The Brand red never draws focus, and no intent restates a ring of its own.
        expect([...actual], `${intent}/${variant}`).not.toContainEqual(expect.stringMatching(/^focus-visible:ring-/));
      }
    }
  });

  it("renders the requested tone through the public component prop", () => {
    for (const tone of TONES) {
      for (const variant of BUTTON_VARIANTS) {
        const element = rendered(render(Button, { tone, variant }));
        expect(classes(element), `${tone}/${variant}`).toEqual(classesOf(button({ tone, variant })));
      }
    }
  });

  it("keeps the deprecated intent as a working alias that warns once in development", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      rendered(render(Button, { tone: "danger" }));
      expect(warn).not.toHaveBeenCalled();
      const aliased = [1, 2].map(() => rendered(render(Button, { intent: "warning", variant: "secondary" })));
      for (const element of aliased) {
        expect(classes(element)).toEqual(classesOf(button({ tone: "warning", variant: "secondary" })));
      }
      expect(warn).toHaveBeenCalledTimes(1);
      expect(String(warn.mock.calls[0]![0])).toMatch(/\[reddb Button\] intent="warning" is deprecated.*tone="warning"/);
    } finally {
      warn.mockRestore();
    }
  });

  it("keeps labels AA on their fill and the ink focus ring 3:1 on every ground, in every appearance", () => {
    // [fill the label sits on, label]. Outlined and ghost intents set their
    // copy on whatever ground they are placed on; the contract measures the
    // feedback copy on every ground, so here they are read on the page and on
    // their own tinted hover surface.
    const pairs: Record<string, Record<string, readonly [string, string][]>> = {
      neutral: {
        primary: [["--reddb-color-primary", "--reddb-color-on-primary"]],
        secondary: [["--reddb-color-background", "--reddb-color-foreground"]],
        ghost: [["--reddb-color-background", "--reddb-color-ink-muted"]],
      },
    };
    for (const role of ["danger", "success", "warning", "info"]) {
      const copy = `--reddb-color-feedback-${role}-foreground`;
      pairs[role] = {
        primary: [[`--reddb-color-feedback-${role}-fill`, `--reddb-color-feedback-${role}-on-fill`]],
        secondary: [["--reddb-color-background", copy], [`--reddb-color-feedback-${role}-surface`, copy]],
        ghost: [["--reddb-color-background", copy], [`--reddb-color-feedback-${role}-surface`, copy]],
      };
    }
    const grounds = ["background", "elevation-sunken-surface", "elevation-base-surface", "elevation-raised-surface", "elevation-overlay-surface"];

    for (const theme of APPEARANCE_THEMES) {
      for (const scheme of COLOR_SCHEMES) {
        const cascade = cascadeFor(composition, [
          { "data-theme": theme.name, "data-color-scheme": scheme.name },
        ]);
        const at = (token: string) => resolveProperty(cascade, token);
        for (const [intent, variants] of Object.entries(pairs)) {
          for (const [variant, surfaces] of Object.entries(variants)) {
            for (const [surfaceToken, labelToken] of surfaces) {
              expect(contrast(at(surfaceToken), at(labelToken)), `${intent}/${variant} label on ${surfaceToken} at ${theme.name}/${scheme.name}`)
                .toBeGreaterThanOrEqual(4.5);
            }
          }
        }
        // The offset ring is drawn on the ground around the Button, never on its fill.
        for (const ground of grounds) {
          expect(contrast(at(`--reddb-color-${ground}`), at("--reddb-color-focus")), `focus ring on ${ground} at ${theme.name}/${scheme.name}`)
            .toBeGreaterThanOrEqual(3);
        }
      }
    }
  });

  it("merges a caller's classes over its own", () => {
    const element = rendered(render(Button, { class: "w-full" }));
    expect(classes(element).has("w-full")).toBe(true);
    expect(classes(element).has("border-control-edge")).toBe(true);
  });

  it("passes native attributes and handlers straight through", () => {
    const onclick = vi.fn();
    const element = rendered(render(Button, { onclick, type: "submit", "aria-pressed": "true" }));
    expect(element.getAttribute("type")).toBe("submit");
    expect(element.getAttribute("aria-pressed")).toBe("true");
    click(element);
    expect(onclick).toHaveBeenCalledTimes(1);
  });

  it("does not fire while disabled", () => {
    const onclick = vi.fn();
    const element = rendered(render(Button, { onclick, disabled: true }));
    expect((element as HTMLButtonElement).disabled).toBe(true);
    click(element);
    expect(onclick).not.toHaveBeenCalled();
  });
});

describe("the Base Button as an anchor", () => {
  it("renders an <a> to wherever href points", () => {
    const element = rendered(render(Button, { href: "/docs", children: text("Read the guide") }));
    expect(element.tagName).toBe("A");
    expect(element.getAttribute("href")).toBe("/docs");
    expect(element.hasAttribute("type")).toBe(false);
  });

  it("wears the same variant classes as a button", () => {
    for (const variant of BUTTON_VARIANTS) {
      for (const size of BUTTON_SIZES) {
        const anchor = rendered(render(Button, { href: "/docs", variant, size }));
        expect(classes(anchor)).toEqual(classesOf(button({ variant, size })));
      }
    }
  });

  it("passes native anchor attributes straight through", () => {
    const element = rendered(
      render(Button, { href: "https://example.test", target: "_blank", rel: "noreferrer" }),
    );
    expect(element.getAttribute("target")).toBe("_blank");
    expect(element.getAttribute("rel")).toBe("noreferrer");
  });

  it("preserves an explicit roving tab stop on native links", () => {
    const link=rendered(render(Button,{href:"/docs",tabindex:-1}));
    expect(link.tabIndex).toBe(-1);
    const disabled=rendered(render(Button,{href:"/docs",tabindex:0,disabled:true}));
    expect(disabled.tabIndex).toBe(-1);
  });

  it("withholds the destination while disabled", () => {
    const element = rendered(render(Button, { href: "/docs", disabled: true }));
    expect(element.hasAttribute("href")).toBe(false);
    expect(element.getAttribute("tabindex")).toBe("-1");
    expect(element.getAttribute("aria-disabled")).toBe("true");
  });
});

describe("the Base Button while loading", () => {
  it("is disabled, announces itself as busy, and does not fire", () => {
    const onclick = vi.fn();
    const element = rendered(render(Button, { onclick, loading: true, children: text("Save") }));
    expect((element as HTMLButtonElement).disabled).toBe(true);
    expect(element.getAttribute("aria-busy")).toBe("true");
    click(element);
    expect(onclick).not.toHaveBeenCalled();
  });

  it("draws an assistive-technology-hidden spinner beside its label", () => {
    const element = rendered(render(Button, { loading: true, children: text("Save") }));
    const spinner = element.querySelector("svg");
    expect(spinner).not.toBeNull();
    expect(classes(spinner!)).toEqual(classesOf(buttonSpinner({ size: "md" }).root()));
    expect(spinner!.getAttribute("aria-hidden")).toBe("true");
    expect(element.textContent).toContain("Save");
  });

  it("sizes the spinner with the button, on the Icon size role of the same name", () => {
    for (const size of BUTTON_SIZES) {
      const spinner = rendered(render(Button, { loading: true, size })).querySelector("svg")!;
      expect(classes(spinner)).toEqual(classesOf(buttonSpinner({ size }).root()));
      // Density-responsive, so it matches a leading Icon at every stop.
      expect(classes(spinner).has(`size-[var(--reddb-spatial-icon-size-${size})]`), size).toBe(true);
    }
  });

  it("draws the spinner in the button's own colour", () => {
    const spinner = rendered(render(Button, { loading: true, variant: "secondary" })).querySelector("svg")!;
    const strokeClasses = [...spinner.querySelectorAll("*")].flatMap((node) => [...classes(node)]);
    expect(strokeClasses).toContain("stroke-current");
    const coloured = [...classes(spinner), ...strokeClasses].filter((name) =>
      /^(text|fill|stroke)-/.test(name),
    );
    expect(coloured.filter((name) => !/-(current|none)$/.test(name))).toEqual([]);
  });

  it("draws no spinner when it is not loading", () => {
    expect(rendered(render(Button, { children: text("Save") })).querySelector("svg")).toBeNull();
  });

  it("takes an anchor out of action too", () => {
    const element = rendered(render(Button, { href: "/docs", loading: true }));
    expect(element.tagName).toBe("A");
    expect(element.hasAttribute("href")).toBe(false);
    expect(element.querySelector("svg")).not.toBeNull();
  });
});

describe("the Base Button as a block", () => {
  it("fills its column when asked, and only then", () => {
    expect(classes(rendered(render(Button, { block: true }))).has("w-full")).toBe(true);
    expect(classes(rendered(render(Button, {}))).has("w-full")).toBe(false);
  });

  it("wears the exported block classes on either native element", () => {
    for (const variant of BUTTON_VARIANTS) {
      const asButton = rendered(render(Button, { variant, block: true }));
      expect(classes(asButton)).toEqual(classesOf(button({ variant, block: true })));

      const asAnchor = rendered(render(Button, { href: "/docs", variant, block: true }));
      expect(classes(asAnchor)).toEqual(classesOf(button({ variant, block: true })));
    }
  });
});
