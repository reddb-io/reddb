import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import {
  APPEARANCE_THEMES,
  BASE_THEME,
  COLOR_SCHEMES,
} from "../../../packages/theme/src/appearance";
import {
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

/** Where a Card can sit: the page and every elevation surface. */
const GROUNDS = ["background", "sunken", "base", "raised", "overlay"].map((ground) =>
  ground === "background" ? "--reddb-color-background" : `--reddb-color-elevation-${ground}-surface`,
);

const composition = parseStylesheets([
  readFileSync(
    join(import.meta.dirname, "..", "..", "..", "packages", "tokens", "dist", "tokens.css"),
    "utf8",
  ),
  readFileSync(appearanceThemeCssPath(BASE_THEME), "utf8"),
  ...APPEARANCE_THEMES.map((theme) =>
    readFileSync(appearanceThemeCssPath(theme), "utf8"),
  ),
  ...COLOR_SCHEMES.map((scheme) => readFileSync(colorSchemeCssPath(scheme), "utf8")),
]);

const contracts = {
  neutral: {
    surface: "--reddb-color-background",
    title: "--reddb-color-foreground",
    description: "--reddb-color-ink-muted",
    body: "--reddb-color-foreground",
  },
  brand: {
    surface: "--reddb-color-primary",
    title: "--reddb-color-on-primary",
    description: "--reddb-color-on-primary",
    body: "--reddb-color-on-primary",
  },
  success: {
    surface: "--reddb-color-feedback-success-surface",
    title: "--reddb-color-feedback-success-foreground",
    description: "--reddb-color-feedback-success-foreground",
    body: "--reddb-color-feedback-success-foreground",
  },
  warning: {
    surface: "--reddb-color-feedback-warning-surface",
    title: "--reddb-color-feedback-warning-foreground",
    description: "--reddb-color-feedback-warning-foreground",
    body: "--reddb-color-feedback-warning-foreground",
  },
  danger: {
    surface: "--reddb-color-feedback-danger-surface",
    title: "--reddb-color-feedback-danger-foreground",
    description: "--reddb-color-feedback-danger-foreground",
    body: "--reddb-color-feedback-danger-foreground",
  },
  info: {
    surface: "--reddb-color-feedback-info-surface",
    title: "--reddb-color-feedback-info-foreground",
    description: "--reddb-color-feedback-info-foreground",
    body: "--reddb-color-feedback-info-foreground",
  },
} as const;

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

describe("the Base Card tone contrast contract", () => {
  it("keeps title, description, and body at normal-text AA in every appearance", () => {
    for (const theme of APPEARANCE_THEMES) {
      for (const scheme of COLOR_SCHEMES) {
        const cascade = cascadeFor(composition, [
          { "data-theme": theme.name, "data-color-scheme": scheme.name },
        ]);

        for (const [tone, contract] of Object.entries(contracts)) {
          // A translucent tone surface (ADR 0019) is measured over every
          // ground the Card can land on.
          const fill = resolveColor(cascade, contract.surface);
          const surfaces =
            fill.alpha === 1
              ? [fill.hex]
              : GROUNDS.map((ground) => over(fill, resolveProperty(cascade, ground)));
          for (const surface of surfaces) {
            for (const region of ["title", "description", "body"] as const) {
              const foreground = resolveProperty(cascade, contract[region]);
              expect(
                contrast(surface, foreground),
                `${tone} ${region} at ${theme.name}/${scheme.name}`,
              ).toBeGreaterThanOrEqual(4.5);
            }
          }
        }
      }
    }
  });
});
