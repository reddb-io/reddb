import { readFileSync } from "node:fs";
import { join } from "node:path";
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import { APPEARANCE_THEMES, BASE_THEME, COLOR_SCHEMES } from "../../../packages/theme/src/appearance";
import { appearanceThemeCssPath, colorSchemeCssPath } from "../../../packages/theme/src/paths";
import {
  cascadeFor,
  parseStylesheets,
  resolveProperty,
} from "../../../packages/theme/test/support/cascade";
import KpiCard, { kpiDeltaSentiment } from "../src/KpiCard.svelte";
import KpiSparklineConsumer from "./fixtures/KpiSparklineConsumer.svelte";
import { preloadChartEngines } from "../src/chart-engines";

// The Sparkline's engine is a dynamic import (wave 5D); loaded up front, it
// draws within the mount's flush where IntersectionObserver does not exist.
beforeAll(() => preloadChartEngines(), 60_000);

const mounted: Array<{ instance: Record<string, unknown>; target: HTMLElement }> = [];

function renderComponent(
  component: Parameters<typeof mount>[0],
  props: Record<string, unknown>,
): HTMLElement {
  const target = document.createElement("div");
  document.body.append(target);
  const instance = mount(component, { target, props }) as Record<string, unknown>;
  mounted.push({ instance, target });
  flushSync();
  return target.querySelector<HTMLElement>("[data-kpi-card]")!;
}

const render = (props: Record<string, unknown>) => renderComponent(KpiCard, props);

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

afterEach(() => {
  for (const entry of mounted.splice(0)) {
    unmount(entry.instance);
    entry.target.remove();
  }
});

describe("KpiCard tones", () => {
  it("routes every tone surface and foreground through its paired DS tokens", () => {
    const contracts = {
      neutral: [
        "bg-elevation-raised-surface",
        "border-muted",
        "text-foreground",
        "text-ink-muted",
      ],
      brand: [
        "bg-[var(--reddb-color-primary)]",
        "text-[var(--reddb-color-on-primary)]",
      ],
      success: [
        "bg-[var(--reddb-color-feedback-success-surface)]",
        "text-[var(--reddb-color-feedback-success-foreground)]",
      ],
      warning: [
        "bg-[var(--reddb-color-feedback-warning-surface)]",
        "text-[var(--reddb-color-feedback-warning-foreground)]",
      ],
      danger: [
        "bg-[var(--reddb-color-feedback-danger-surface)]",
        "text-[var(--reddb-color-feedback-danger-foreground)]",
      ],
      info: [
        "bg-[var(--reddb-color-feedback-info-surface)]",
        "text-[var(--reddb-color-feedback-info-foreground)]",
      ],
    } as const;

    for (const [tone, tokens] of Object.entries(contracts)) {
      const card = render({ label: `${tone} metric`, value: 42, tone });
      const classes = `${card.className} ${card.querySelector("[data-kpi-label]")?.className}`;

      expect(card.dataset.tone).toBe(tone);
      for (const token of tokens) expect(classes, tone).toContain(token);
      if (tone === "neutral") {
        expect(classes).toContain("bg-elevation-raised-surface");
        expect(classes).not.toContain("border-elevation-raised-border");
        expect(classes).not.toContain("shadow-elevation-raised");
      }
    }
  });

  it("keeps semantic delta color on neutral and inherits on-tone color elsewhere", () => {
    const positive = render({ label: "Growth", value: 42, delta: 3 });
    const negative = render({ label: "Errors", value: 42, delta: -3 });

    expect(positive.querySelector("[data-kpi-delta]")?.className).toContain(
      "text-feedback-success-foreground",
    );
    // The delta never selects the Theme axis (ADR 0003/0008): it is legible
    // on the card's own ground in every direction.
    expect(positive.querySelector("[data-kpi-delta]")?.className).not.toContain("data-theme");
    expect(negative.querySelector("[data-kpi-delta]")?.className).toContain(
      "text-feedback-danger-foreground",
    );

    for (const tone of ["brand", "success", "warning", "danger", "info"]) {
      const card = render({ label: `${tone} growth`, value: 42, delta: 3, tone });
      const delta = card.querySelector<HTMLElement>("[data-kpi-delta]")!;

      expect(delta.className, tone).not.toMatch(/text-(?:feedback|ink-muted)/);
      expect(card.dataset.deltaDirection).toBe("increase");
      expect(delta.textContent).toContain("versus previous period");
    }
  });

  it("colours a delta by what it means under the card's polarity, not by its sign", () => {
    expect(kpiDeltaSentiment(3)).toBe("improvement");
    expect(kpiDeltaSentiment(-3)).toBe("regression");
    expect(kpiDeltaSentiment(-17, "lower-is-better")).toBe("improvement");
    expect(kpiDeltaSentiment(4, "lower-is-better")).toBe("regression");
    expect(kpiDeltaSentiment(8.6, "neutral")).toBe("neutral");
    expect(kpiDeltaSentiment(0, "lower-is-better")).toBe("neutral");

    const latency = render({ label: "P95 latency", value: "151 ms", delta: -17, polarity: "lower-is-better" });
    const errors = render({ label: "Error rate", value: "0.4%", delta: 0.2, polarity: "lower-is-better" });
    const traffic = render({ label: "Requests", value: "84k", delta: 8.6, polarity: "neutral" });
    const growth = render({ label: "Signups", value: 42, delta: 3 });

    expect(latency.dataset.deltaDirection).toBe("decrease");
    expect(latency.dataset.deltaSentiment).toBe("improvement");
    expect(latency.dataset.polarity).toBe("lower-is-better");
    expect(latency.querySelector("[data-kpi-delta]")?.className).toContain("text-feedback-success-foreground");
    expect(errors.dataset.deltaSentiment).toBe("regression");
    expect(errors.querySelector("[data-kpi-delta]")?.className).toContain("text-feedback-danger-foreground");
    expect(traffic.dataset.deltaSentiment).toBe("neutral");
    expect(traffic.querySelector("[data-kpi-delta]")?.className).toContain("text-ink-muted");
    expect(traffic.querySelector("[data-kpi-delta]")?.className).not.toMatch(/text-feedback/);
    expect(growth.dataset.polarity).toBe("higher-is-better");
    expect(growth.dataset.deltaSentiment).toBe("improvement");
    expect(latency.querySelector("[data-kpi-value]")?.className).toContain("tabular-nums");
  });

  it("preserves visible comparison context when the caller also supplies a description", () => {
    const card = render({ label: "Spend", value: "$428.70", delta: -12.4, polarity: "lower-is-better", deltaLabel: "vs. previous 7 days", description: "Compute, storage and network" });
    const comparison = card.querySelector<HTMLElement>("[data-kpi-comparison]")!;
    expect(comparison.textContent).toBe("vs. previous 7 days");
    expect(comparison.className).not.toContain("sr-only");
    expect(card.querySelector("[data-kpi-description]")?.textContent).toBe("Compute, storage and network");
    expect(card.querySelector("[data-kpi-delta] .sr-only")?.textContent?.trim()).toBe("vs. previous 7 days");
    expect(comparison.getAttribute("aria-hidden")).toBe("true");
  });

  it("keeps label, value, and delta contrast at normal-text AA across both appearance axes", () => {
    const tonePairs = {
      brand: ["--reddb-color-primary", "--reddb-color-on-primary"],
      success: [
        "--reddb-color-feedback-success-surface",
        "--reddb-color-feedback-success-foreground",
      ],
      warning: [
        "--reddb-color-feedback-warning-surface",
        "--reddb-color-feedback-warning-foreground",
      ],
      danger: [
        "--reddb-color-feedback-danger-surface",
        "--reddb-color-feedback-danger-foreground",
      ],
      info: ["--reddb-color-feedback-info-surface", "--reddb-color-feedback-info-foreground"],
    } as const;

    for (const theme of APPEARANCE_THEMES) {
      for (const scheme of COLOR_SCHEMES) {
        const appearance = `${theme.name}/${scheme.name}`;
        const cascade = cascadeFor(composition, [
          { "data-theme": theme.name, "data-color-scheme": scheme.name },
        ]);

        const neutralSurface = resolveProperty(cascade, "--reddb-color-background");
        for (const foreground of ["--reddb-color-foreground", "--reddb-color-ink-muted"]) {
          expect(contrast(neutralSurface, resolveProperty(cascade, foreground)), appearance)
            .toBeGreaterThanOrEqual(4.5);
        }

        for (const role of ["success", "danger"] as const) {
          const deltaSurface = resolveProperty(cascade, "--reddb-color-background");
          const deltaForeground = resolveProperty(
            cascade,
            `--reddb-color-feedback-${role}-foreground`,
          );
          expect(contrast(deltaSurface, deltaForeground), `${role} delta at ${appearance}`)
            .toBeGreaterThanOrEqual(4.5);
        }

        for (const [tone, [surfaceToken, foregroundToken]] of Object.entries(tonePairs)) {
          const surface = resolveProperty(cascade, surfaceToken);
          const foreground = resolveProperty(cascade, foregroundToken);
          expect(contrast(surface, foreground), `${tone} at ${appearance}`)
            .toBeGreaterThanOrEqual(4.5);
        }
      }
    }
  });

  it("leaves a neutral card's Sparkline in its own Series Role, never the Brand red", () => {
    const card = renderComponent(KpiSparklineConsumer, { tone: "neutral" });
    const chart = card.querySelector<HTMLElement>('[data-chart-kind="sparkline"]')!;

    expect(card.className).not.toContain("--color-primary");
    expect(chart.className).toContain("[--color-primary:var(--reddb-color-series-1)]");
    expect(chart.querySelector('[stroke="var(--color-primary)"]')).not.toBeNull();
  });

  it("recolors a rendered Sparkline on a filled tone through its CSS variable bridge", () => {
    const contracts = {
      brand: "--reddb-color-on-primary",
      success: "--reddb-color-feedback-success-foreground",
      warning: "--reddb-color-feedback-warning-foreground",
      danger: "--reddb-color-feedback-danger-foreground",
      info: "--reddb-color-feedback-info-foreground",
    } as const;

    for (const [tone, foreground] of Object.entries(contracts)) {
      const card = renderComponent(KpiSparklineConsumer, { tone });
      const chart = card.querySelector<HTMLElement>('[data-chart-kind="sparkline"]')!;

      expect(card.dataset.tone).toBe(tone);
      expect(card.className).toContain(
        `[&_[data-ds-chart]]:[--color-primary:var(${foreground})]`,
      );
      expect(chart.querySelector('[stroke="var(--color-primary)"]')).not.toBeNull();
    }
  });
});
