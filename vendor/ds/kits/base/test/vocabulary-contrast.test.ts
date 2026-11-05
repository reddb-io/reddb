// Every colour a component's vocabulary offers, measured where it lands.
//
// The Foundation's contrast contract (packages/theme) measures roles; it cannot
// see which role a component's prop resolves to. Icon offered `color="muted"`
// and the neutral StatusIndicator drew its mark in `muted` — a surface role,
// ink at 8% since ADR 0019 — and both painted at 1.19:1 while every role-level
// test stayed green. So this contract mounts each component with every value of
// its colour vocabulary, reads the paint it actually carries, resolves that
// through the generated Theme × Color Scheme artifacts and measures it on every
// ground it can sit on. A glyph or a status mark is a non-text graphic: 3:1
// (WCAG 1.4.11).

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { createRawSnippet } from "svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  APPEARANCE_THEMES,
  BASE_THEME,
  COLOR_SCHEMES,
} from "../../../packages/theme/src/appearance";
import {
  DIST_DIR,
  appearanceThemeCssPath,
  colorSchemeCssPath,
} from "../../../packages/theme/src/paths";
import {
  cascadeFor,
  over,
  parseStylesheets,
  resolveColor,
  resolveProperty,
} from "../../../packages/theme/test/support/cascade";
import {
  Alert,
  BADGE_VARIANTS,
  Badge,
  Button,
  Card,
  ICON_COLORS,
  Icon,
  STATUS_INDICATOR_STATUSES,
  StatusIndicator,
  TONES,
} from "../src/index";
import TestGlyph from "./fixtures/TestGlyph.svelte";
import { classes, render, rendered } from "./mount";

const NON_TEXT_AA = 3;

/** Every opaque ground a Kit places a graphic on: the page and each elevation. */
const GROUNDS = ["background", "sunken", "base", "raised", "overlay"].map((ground) =>
  ground === "background" ? "--reddb-color-background" : `--reddb-color-elevation-${ground}-surface`,
);

const composition = parseStylesheets([
  readFileSync(
    join(import.meta.dirname, "..", "..", "..", "packages", "tokens", "dist", "tokens.css"),
    "utf8",
  ),
  readFileSync(appearanceThemeCssPath(BASE_THEME), "utf8"),
  ...APPEARANCE_THEMES.map((theme) => readFileSync(appearanceThemeCssPath(theme), "utf8")),
  ...COLOR_SCHEMES.map((scheme) => readFileSync(colorSchemeCssPath(scheme), "utf8")),
]);

/** The Tailwind @theme surface: a named colour utility -> the role it paints. */
const UTILITY_ROLES = new Map(
  [
    ...readFileSync(join(DIST_DIR, "theme.css"), "utf8").matchAll(
      /--color-([a-z0-9-]+):\s*var\((--reddb-color-[a-z0-9-]+)\)/g,
    ),
  ].map(([, utility, role]) => [utility!, role!]),
);

const VAR_ROLE_RE = /^var\((--reddb-color-[a-z0-9-]+)\)$/;

/** The role a `var(--reddb-color-*)` paint value draws in. */
function roleOfPaint(value: string | null): string {
  const role = VAR_ROLE_RE.exec(value ?? "")?.[1];
  if (!role) throw new Error(`paint "${value}" is not a DS colour role`);
  return role;
}

/** The single colour role an element's `bg-*` or `border-*` utility paints. */
function roleOfUtility(element: Element, utility: "bg" | "border" | "text"): string {
  const roles = [...classes(element)].flatMap((name) => {
    if (!name.startsWith(`${utility}-`)) return [];
    const value = name.slice(utility.length + 1);
    const arbitrary = /^\[(var\(--reddb-color-[a-z0-9-]+\))\]$/.exec(value)?.[1];
    if (arbitrary) return [roleOfPaint(arbitrary)];
    const named = UTILITY_ROLES.get(value);
    return named ? [named] : [];
  });
  expect(roles, `${utility} colour of <${element.tagName.toLowerCase()} class="${element.getAttribute("class")}">`).toHaveLength(1);
  return roles[0]!;
}

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

/** Every Theme × Color Scheme a component renders in, with its resolved cascade. */
function appearances() {
  return APPEARANCE_THEMES.flatMap((theme) =>
    COLOR_SCHEMES.map((scheme) => ({
      name: `${theme.name}/${scheme.name}`,
      cascade: cascadeFor(composition, [
        { "data-theme": theme.name, "data-color-scheme": scheme.name },
      ]),
    })),
  );
}

describe("the Icon colour vocabulary contrast contract", () => {
  // The deprecated alias warns once in development; the warning has its own test.
  beforeEach(() => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  // `current` inherits the ink of whatever it sits in, which that context's own
  // contract measures. `on-primary` is ink for a glyph on a primary fill.
  // `muted` is still accepted for one release, so it is held to the same floor.
  const measured = [...ICON_COLORS.filter((color) => color !== "current"), "muted"] as const;
  const groundsOf = (color: string) => (color === "on-primary" ? ["--reddb-color-primary"] : GROUNDS);

  it("draws every colour it accepts at 3:1 on every ground, in every appearance", () => {
    const failures: string[] = [];
    for (const color of measured) {
      const glyph = rendered(
        render(Icon, { icon: TestGlyph, color: color as (typeof ICON_COLORS)[number], "aria-hidden": "true" }),
      );
      const role = roleOfPaint(glyph.getAttribute("stroke"));
      for (const { name, cascade } of appearances()) {
        const paint = resolveColor(cascade, role);
        for (const groundRole of groundsOf(color)) {
          const ground = resolveProperty(cascade, groundRole);
          const ratio = contrast(over(paint, ground), ground);
          if (ratio < NON_TEXT_AA) {
            failures.push(`color="${color}" (${role}) on ${groundRole} at ${name}: ${ratio.toFixed(2)}:1`);
          }
        }
      }
    }
    expect(failures).toEqual([]);
  });
});

describe("the StatusIndicator tone contrast contract", () => {
  it("draws every status mark at 3:1 on every ground, in every appearance", () => {
    const failures: string[] = [];
    for (const status of STATUS_INDICATOR_STATUSES) {
      const mark = rendered(render(StatusIndicator, { label: `${status} status`, tone: status }))
        .querySelector("[data-status-mark]")!;
      const fillRole = roleOfUtility(mark, "bg");
      const edgeRole = roleOfUtility(mark, "border");
      for (const { name, cascade } of appearances()) {
        const fill = resolveColor(cascade, fillRole);
        const edge = resolveColor(cascade, edgeRole);
        for (const groundRole of GROUNDS) {
          const ground = resolveProperty(cascade, groundRole);
          // The fill runs under the 1px edge (background-clip: border-box), so
          // the ring is the edge composited over the fill over the ground.
          const centre = over(fill, ground);
          const ring = over(edge, centre);
          for (const [part, paint] of [["fill", centre], ["edge", ring]] as const) {
            const ratio = contrast(paint, ground);
            if (ratio < NON_TEXT_AA) {
              failures.push(`${status} ${part} on ${groundRole} at ${name}: ${ratio.toFixed(2)}:1`);
            }
          }
        }
      }
    }
    expect(failures).toEqual([]);
  });
});

// One vocabulary (ADR 0026): a tone names the same Feedback Role on every
// component, and the copy each tone sets holds AA on the surface it paints, in
// every Theme × Color Scheme. A transparent surface (an outlined Badge, a
// secondary Button) is measured on every ground it can sit on; a translucent
// one (the neutral muted surface, ADR 0019) composited over each ground.
describe("the tone text contrast contract", () => {
  const NORMAL_TEXT_AA = 4.5;
  const text = createRawSnippet(() => ({ render: () => "<span>Status</span>" }));

  function paintRoles(element: Element): { copy: string; surface: string | null } {
    const painted = [...classes(element)].some((name) => name.startsWith("bg-") && name !== "bg-transparent");
    const surface = painted ? roleOfUtility(element, "bg") : null;
    return { copy: roleOfUtility(element, "text"), surface };
  }

  function measure(label: string, element: Element, failures: string[]): void {
    const { copy, surface } = paintRoles(element);
    for (const { name, cascade } of appearances()) {
      const ink = resolveColor(cascade, copy);
      for (const groundRole of GROUNDS) {
        const ground = resolveProperty(cascade, groundRole);
        const fill = surface === null ? ground : over(resolveColor(cascade, surface), ground);
        const ratio = contrast(over(ink, fill), fill);
        if (ratio < NORMAL_TEXT_AA) {
          failures.push(`${label}: ${copy} on ${surface ?? groundRole} over ${groundRole} at ${name}: ${ratio.toFixed(2)}:1`);
        }
      }
    }
  }

  beforeEach(() => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("resolves every tone to the same Feedback Role on Badge, Alert, Card and Button", () => {
    for (const tone of TONES.filter((value) => value !== "neutral")) {
      const roles = [
        roleOfUtility(rendered(render(Badge, { tone, children: text })), "text"),
        roleOfUtility(rendered(render(Alert, { tone })), "text"),
        roleOfUtility(rendered(render(Card, { tone, title: "Title" })), "text"),
        roleOfUtility(rendered(render(Button, { tone, variant: "ghost" })), "text"),
      ];
      expect(new Set(roles), tone).toEqual(new Set([`--reddb-color-feedback-${tone}-foreground`]));
    }
  });

  it("keeps every Badge tone at every emphasis at AA, in every appearance", () => {
    const failures: string[] = [];
    for (const variant of BADGE_VARIANTS) {
      for (const tone of TONES) {
        measure(`Badge ${tone}/${variant}`, rendered(render(Badge, { tone, variant, children: text })), failures);
      }
    }
    expect(failures).toEqual([]);
  });

  it("keeps every Alert and Card tone at AA, in every appearance", () => {
    const failures: string[] = [];
    for (const tone of TONES) {
      measure(`Alert ${tone}`, rendered(render(Alert, { tone })), failures);
      measure(`Card ${tone}`, rendered(render(Card, { tone, title: "Title" })), failures);
    }
    expect(failures).toEqual([]);
  });
});
