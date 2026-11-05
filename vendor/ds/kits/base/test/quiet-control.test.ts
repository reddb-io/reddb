// The interaction-state contract for quiet controls (wave 6A).
import { describe, expect, it } from "vitest";
import * as base from "../src/index";
import { button } from "../src/button.variants";
import {
  QUIET_FOCUS,
  QUIET_HOVER,
  QUIET_OPEN,
  QUIET_PRESSED,
  quietControl,
} from "../src/quiet-control.variants";

const set = (classes: string) => new Set(classes.split(/\s+/).filter(Boolean));

describe("quietControl, the one interaction-state contract", () => {
  it("draws rest, hover, pressed, open, focus and disabled the way DESIGN.md names them", () => {
    const names = set(quietControl());
    for (const name of [
      "bg-transparent",
      "hover:bg-foreground/8",
      "active:bg-foreground/12",
      "[&[aria-haspopup][aria-expanded=true]]:bg-foreground/10",
      "focus-visible:outline-2",
      "focus-visible:outline-offset-2",
      "focus-visible:outline-focus",
      "disabled:pointer-events-none",
      "disabled:opacity-50",
      "aria-disabled:opacity-50",
      "text-ink-muted",
      "hover:text-foreground",
    ]) {
      expect(names.has(name), name).toBe(true);
    }
    // States land at once: no transition eases a colour (or the focus outline).
    expect([...names].some((name) => name.startsWith("transition"))).toBe(false);
    // No opacity is ever a hover or press state.
    expect([...names].some((name) => /(?:hover|active):opacity-/.test(name))).toBe(false);
  });

  it("makes pressed stronger than hover, and open between them", () => {
    const alpha = (state: string) => Number(/\/(\d+)$/.exec(state)![1]);
    expect(alpha(QUIET_PRESSED)).toBeGreaterThan(alpha(QUIET_HOVER));
    expect(alpha(QUIET_OPEN)).toBeGreaterThan(alpha(QUIET_HOVER));
    expect(alpha(QUIET_OPEN)).toBeLessThan(alpha(QUIET_PRESSED));
  });

  it("keeps a selected item on the selected surface under the pointer", () => {
    const names = set(quietControl({ selected: true }));
    expect(names.has("bg-foreground/10")).toBe(true);
    expect(names.has("hover:bg-foreground/10")).toBe(true);
    expect(names.has("hover:bg-foreground/8")).toBe(false);
    expect(names.has("font-medium")).toBe(true);
  });

  it("insets the focus outline where a scroll region would clip it", () => {
    const names = set(quietControl({ focus: "inset" }));
    expect(names.has("focus-visible:-outline-offset-2")).toBe(true);
    expect(names.has("focus-visible:outline-offset-2")).toBe(false);
  });

  it("lets a slot's geometry and resting colour win while the states stay", () => {
    const names = set(quietControl({ ink: "foreground", class: "rounded-md bg-background px-[var(--reddb-spatial-inset-sm)]" }));
    expect(names.has("bg-background")).toBe(true);
    expect(names.has("bg-transparent")).toBe(false);
    expect(names.has("hover:bg-foreground/8")).toBe(true);
    expect(names.has("text-foreground")).toBe(true);
  });

  it("is what a ghost Button wears", () => {
    const ghost = set(button({ variant: "ghost" }));
    for (const state of [QUIET_HOVER, QUIET_PRESSED, QUIET_OPEN, ...QUIET_FOCUS.split(" ")]) {
      expect(ghost.has(state), state).toBe(true);
    }
  });

  it("gives every filled Button an opaque active fill, never an opacity", () => {
    for (const tone of ["neutral", "info", "success", "warning", "danger"] as const) {
      const names = [...set(button({ variant: "primary", tone }))];
      expect(names.some((name) => /opacity-/.test(name) && /hover|active/.test(name)), tone).toBe(false);
      const active = tone === "neutral" ? "bg-primary-active" : `bg-feedback-${tone}-fill-active`;
      for (const state of ["hover", "active", "aria-expanded"]) expect(names, `${tone} ${state}`).toContain(`${state}:${active}`);
    }
  });

  it("is exported to every child Kit through the Base barrel", () => {
    expect(base.quietControl).toBe(quietControl);
    expect(base.QUIET_HOVER).toBe(QUIET_HOVER);
  });
});
